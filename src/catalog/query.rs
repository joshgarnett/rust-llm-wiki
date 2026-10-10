//! Selected, bounded reads from an ordinary pinned catalog generation.
//!
//! This reader deliberately does not audit cache completeness or prove canonical
//! freshness. A coordinator must verify selected canonical evidence separately.
use super::{
    Catalog, CatalogDiagnostic, DocumentRow, RecordRow, SnapshotVerification, normalized_read,
    normalized_schema,
    query_types::{QueryCatalog, QueryReadLimits, QueryReadUsage},
    scan, selector, sql,
};
use crate::changes::operation_authority::{Authority, Publication};
use crate::domain::{
    Blake3Hash, Eligibility, ErrorCode, ReadSnapshot, RecordId, RecordKind, Result,
    VaultRelativePath, WikiError,
};
use rusqlite::{Connection, OpenFlags, Row, limits::Limit, params, types::ValueRef};
use serde::de::DeserializeOwned;
use std::{
    cell::Cell,
    collections::BTreeSet,
    ops::Deref,
    time::{Duration, Instant},
};

enum QueryConnection {
    Legacy(Connection),
    Normalized(selector::Selected<Connection>),
}

impl Deref for QueryConnection {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        match self {
            Self::Legacy(connection) => connection,
            Self::Normalized(selected) => selected.value(),
        }
    }
}

pub(crate) struct QuerySnapshot {
    connection: QueryConnection,
    snapshot: ReadSnapshot,
    vault_id: RecordId,
    verification: SnapshotVerification,
    usage: Cell<QueryReadUsage>,
    pub(super) policy_layout_verified: Cell<bool>,
    limits: QueryReadLimits,
    query_started: Instant,
    operation_authority: Option<Authority>,
    scope: QueryScope,
}

#[derive(Clone, Copy)]
enum QueryScope {
    IndexedEvidence,
    CatalogSnapshot,
}

/// Lifecycle-only access deliberately excludes document and FTS text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DocumentMetadata {
    pub path: VaultRelativePath,
    pub record_id: Option<RecordId>,
    pub kind: Option<RecordKind>,
    pub source_id: Option<RecordId>,
    pub owner_revision: Option<RecordId>,
    pub eligibility: Eligibility,
    pub reasons: Vec<String>,
}

/// Scalar adjacency selection; a sentinel proves only that more IDs exist.
pub(crate) struct BoundedIncidentIds {
    pub ids: Vec<RecordId>,
    pub has_more: bool,
}

impl Catalog {
    pub(crate) fn query_snapshot(&self, limits: QueryReadLimits) -> Result<QuerySnapshot> {
        self.query_snapshot_with_scope(limits, QueryScope::IndexedEvidence)
    }

    /// General cached discovery has a distinct cursor/dependency domain from
    /// the selected captured-source evidence protocol. Neither scope audits the
    /// whole catalog or establishes canonical freshness by opening a reader.
    pub(crate) fn cached_query_snapshot(&self, limits: QueryReadLimits) -> Result<QuerySnapshot> {
        self.query_snapshot_with_scope(limits, QueryScope::CatalogSnapshot)
    }

    fn query_snapshot_with_scope(
        &self,
        limits: QueryReadLimits,
        scope: QueryScope,
    ) -> Result<QuerySnapshot> {
        // Reject invalid requests before resolving or opening any cache path.
        limits.validate()?;
        if self.options.busy_timeout_ms > 30_000 {
            return Err(WikiError::new(
                ErrorCode::ConfigInvalid,
                "catalog busy timeout exceeds 30 seconds",
            ));
        }
        let operation_authority = self.operation_state()?;
        let timeout = self.options.busy_timeout_ms.min(limits.max_elapsed_ms);
        let mut selected_header = None;
        if let Some(selected) = selector::acquire(
            &self.fs,
            &self.vault_id,
            Duration::from_millis(timeout),
            |path, selection| {
                #[cfg(test)]
                super::query_diagnostics::access("catalog_open");
                let connection = Connection::open_with_flags(
                    path,
                    OpenFlags::SQLITE_OPEN_READ_ONLY
                        | OpenFlags::SQLITE_OPEN_NO_MUTEX
                        | OpenFlags::SQLITE_OPEN_NOFOLLOW,
                )
                .map_err(sql::sql_error)?;
                let query_started = configure_query(&connection, &limits)?;
                sql::configure(&connection, timeout, false)?;
                connection
                    .execute_batch("BEGIN DEFERRED")
                    .map_err(sql::sql_error)?;
                let header = normalized_read::header(&connection, selection)?;
                if header.snapshot.parser_fingerprint != scan::parser_fingerprint() {
                    return Err(WikiError::new(
                        ErrorCode::OfflineUnavailable,
                        "catalog parser fingerprint changed; run index sync or rebuild",
                    ));
                }
                selected_header = Some((header.snapshot, query_started));
                Ok(connection)
            },
        )? {
            let (snapshot, query_started) =
                selected_header.expect("successful selection reads its header");
            let authority = operation_authority.as_ref().ok_or_else(|| {
                WikiError::new(
                    ErrorCode::RecoveryRequired,
                    "selected normalized catalog has no captured operation authority",
                )
            })?;
            authority.require_read_publication(&Publication {
                file_id: selected.selection().file_id.clone(),
                epoch: snapshot.generation,
            })?;
            self.operation_state()?.ok_or_else(|| {
                WikiError::new(
                    ErrorCode::RecoveryRequired,
                    "selected operation authority disappeared",
                )
            })?;
            return Ok(QuerySnapshot {
                snapshot,
                connection: QueryConnection::Normalized(selected),
                vault_id: self.vault_id.clone(),
                verification: SnapshotVerification::IndexSnapshot,
                usage: Cell::new(QueryReadUsage::default()),
                policy_layout_verified: Cell::new(false),
                limits,
                query_started,
                operation_authority,
                scope,
            });
        }
        if operation_authority.is_some() {
            return Err(WikiError::new(
                ErrorCode::RecoveryRequired,
                "operation authority exists without a selected normalized catalog",
            ));
        }
        let path = self.cache_path()?;
        if !path.exists() {
            return Err(WikiError::new(
                ErrorCode::OfflineUnavailable,
                "catalog cache is absent",
            ));
        }
        // Install limits before configure/header SQL. These are cooperative SQL
        // safeguards, not a hard wall-clock or process-memory guarantee.
        #[cfg(test)]
        super::query_diagnostics::access("catalog_open");
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(sql::sql_error)?;
        let query_started = configure_query(&connection, &limits)?;
        sql::configure(
            &connection,
            self.options.busy_timeout_ms.min(limits.max_elapsed_ms),
            false,
        )?;
        connection
            .execute_batch("BEGIN DEFERRED")
            .map_err(sql::sql_error)?;
        sql::validate_header(&connection)?;
        let mut statement = connection
            .prepare(
                "SELECT g.gen,g.parser_hash,g.manifest_hash FROM index_meta m \
             JOIN generations g ON g.gen=m.published_gen \
             WHERE m.singleton=1 AND g.state='complete'",
            )
            .map_err(sql::sql_error)?;
        let mut rows = statement.query([]).map_err(sql::sql_error)?;
        let row = rows.next().map_err(sql::sql_error)?.ok_or_else(|| {
            WikiError::new(
                ErrorCode::OfflineUnavailable,
                "catalog has no published generation",
            )
        })?;
        let generation: i64 = row.get(0).map_err(sql::sql_error)?;
        if generation <= 0 {
            return Err(corrupt("catalog generation must be positive"));
        }
        // Hash fields have a fixed encoded size; never copy an unbounded header.
        let parser = header_hash(row, 1)?;
        let manifest = header_hash(row, 2)?;
        if parser != scan::parser_fingerprint() {
            return Err(WikiError::new(
                ErrorCode::OfflineUnavailable,
                "catalog parser fingerprint changed; run index sync or rebuild",
            ));
        }
        let snapshot = ReadSnapshot::canonical(generation as u64, parser, manifest);
        drop(rows);
        drop(statement);
        Ok(QuerySnapshot {
            connection: QueryConnection::Legacy(connection),
            snapshot,
            // Schema v1 does not have a vault identity header. Selected canonical
            // proof must establish this caller-supplied identity before citations.
            vault_id: self.vault_id.clone(),
            verification: SnapshotVerification::IndexSnapshot,
            usage: Cell::new(QueryReadUsage::default()),
            policy_layout_verified: Cell::new(false),
            limits,
            query_started,
            operation_authority: None,
            scope,
        })
    }
}

fn configure_query(connection: &Connection, limits: &QueryReadLimits) -> Result<Instant> {
    let start = Instant::now();
    let elapsed = Duration::from_millis(limits.max_elapsed_ms);
    // SQLite may do substantial work inside a native FTS operation between
    // callbacks. Count executed VM work cumulatively across this connection.
    // SQLite can reset its interval counter for newly prepared short statements.
    // A one-op callback is required for a cumulative connection allowance.
    let interval = 1;
    let mut remaining = limits.max_vm_steps;
    connection
        .progress_handler(
            interval,
            Some(move || {
                #[cfg(test)]
                super::query_diagnostics::vm_step();
                if start.elapsed() >= elapsed || remaining <= interval as u64 {
                    return true;
                }
                remaining -= interval as u64;
                false
            }),
        )
        .map_err(sql::sql_error)?;
    // SQLite's row/value ceiling includes duplicated projected text fields;
    // it is deliberately distinct from the lower JSON deserialization ceiling.
    connection
        .set_limit(Limit::SQLITE_LIMIT_LENGTH, 32 * 1024 * 1024)
        .map_err(sql::sql_error)?;
    connection
        .set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, 256 * 1024)
        .map_err(sql::sql_error)?;
    connection
        .execute_batch("PRAGMA mmap_size=0; PRAGMA cache_size=-8192; PRAGMA temp_store=FILE;")
        .map_err(sql::sql_error)?;
    Ok(start)
}

fn corrupt(message: impl Into<String>) -> WikiError {
    WikiError::new(ErrorCode::IndexCorrupt, message)
}

fn text_bytes<'a>(row: &'a Row<'_>, column: usize) -> Result<&'a [u8]> {
    match row.get_ref(column).map_err(sql::sql_error)? {
        ValueRef::Text(bytes) => Ok(bytes),
        _ => Err(corrupt("catalog selected field is not SQLite text")),
    }
}

fn utf8(bytes: &[u8]) -> Result<&str> {
    std::str::from_utf8(bytes).map_err(|_| corrupt("catalog selected text is not UTF-8"))
}

fn header_hash(row: &Row<'_>, column: usize) -> Result<Blake3Hash> {
    let bytes = text_bytes(row, column)?;
    if bytes.len() != 71 {
        return Err(corrupt("catalog header hash has invalid length"));
    }
    Blake3Hash::new(utf8(bytes)?).map_err(|_| corrupt("catalog header hash is invalid"))
}

impl QuerySnapshot {
    pub(crate) fn verify_operations(&self, catalog: &Catalog) -> Result<()> {
        if let Some(captured) = &self.operation_authority {
            catalog.operation_state()?.ok_or_else(|| {
                WikiError::new(
                    ErrorCode::RecoveryRequired,
                    "selected operation authority disappeared",
                )
            })?;
            let QueryConnection::Normalized(selected) = &self.connection else {
                return Err(corrupt(
                    "operation authority requires a normalized selected catalog",
                ));
            };
            // The floor observed at query start remains the read contract.
            // New operations may advance authority while this SQL transaction
            // stays coherent; selected canonical dependencies are checked later.
            captured.require_read_publication(&Publication {
                file_id: selected.selection().file_id.clone(),
                epoch: self.snapshot.generation,
            })?;
            return Ok(());
        }
        if self.normalized_layout() {
            return Err(WikiError::new(
                ErrorCode::RecoveryRequired,
                "normalized selected catalog lacks captured operation authority",
            ));
        }
        catalog.guard_current(None)?;
        if catalog.operation_state()?.is_some() {
            return Err(WikiError::new(
                ErrorCode::RecoveryRequired,
                "normalized authority activated while a legacy reader was held",
            ));
        }
        Ok(())
    }

    /// Select a conservative identity reservation without requiring adoption or
    /// canonical freshness. Only the requested key and two ambiguity witnesses
    /// are decoded; the caller checks the selected document/record binding.
    pub(crate) fn unique_identity_claim(
        &self,
        id: &RecordId,
    ) -> Result<Option<super::IdentityClaimRow>> {
        self.require_refresh_publication()?;
        self.reserve_fact_input(id.as_str().len())?;
        self.require_identity_claim_access()?;
        let mut statement = self.connection.prepare(
            "SELECT record_id,path,file_hash,kind FROM identity_claims INDEXED BY sqlite_autoindex_identity_claims_1 WHERE record_id=?1 ORDER BY path LIMIT 2",
        ).map_err(sql::sql_error)?;
        let mut rows = statement.query([id.as_str()]).map_err(sql::sql_error)?;
        let mut selected: Option<super::IdentityClaimRow> = None;
        while let Some(row) = rows.next().map_err(sql::sql_error)? {
            self.reserve_refresh_row(row, 4)?;
            let claim = decode_identity_claim(row, id)?;
            let path = &claim.path;
            if let Some(first) = &selected {
                if &first.path == path {
                    return Err(corrupt(
                        "identity claim primary key returned duplicate paths",
                    ));
                }
                return Err(WikiError::new(
                    ErrorCode::ReferenceAmbiguous,
                    "record ID has multiple canonical identity claims",
                ));
            }
            selected = Some(claim);
        }
        Ok(selected)
    }

    /// Complete selected identity claims, including malformed-note reservations
    /// and every duplicate claimant path. Completeness fails with the read budget.
    pub(crate) fn identity_claims_for_id(
        &self,
        id: &RecordId,
    ) -> Result<Vec<super::IdentityClaimRow>> {
        self.require_refresh_publication()?;
        self.reserve_fact_input(id.as_str().len())?;
        self.require_identity_claim_access()?;
        let mut statement = self.connection.prepare(
            "SELECT record_id,path,file_hash,kind FROM identity_claims INDEXED BY sqlite_autoindex_identity_claims_1 WHERE record_id=?1 ORDER BY path",
        ).map_err(sql::sql_error)?;
        let mut rows = statement.query([id.as_str()]).map_err(sql::sql_error)?;
        let mut result: Vec<super::IdentityClaimRow> = Vec::new();
        while let Some(row) = rows.next().map_err(sql::sql_error)? {
            self.reserve_refresh_row(row, 4)?;
            let claim = decode_identity_claim(row, id)?;
            if result
                .last()
                .is_some_and(|previous| previous.path >= claim.path)
            {
                return Err(corrupt("identity claim paths are not unique sorted keys"));
            }
            result.push(claim);
        }
        Ok(result)
    }

    fn require_identity_claim_access(&self) -> Result<()> {
        let unavailable = || {
            let mut error = WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "catalog lacks the bounded identity claim primary key",
            );
            error.hint = Some("run index rebuild".into());
            error
        };
        // An exact bounded schema witness rejects alternative collations,
        // partial substitutes and extra key columns before preparing the probe.
        let mut statement = self.connection.prepare(
            "SELECT sql FROM sqlite_schema WHERE type='table' AND name='identity_claims' LIMIT 2",
        ).map_err(sql::sql_error)?;
        let mut rows = statement.query([]).map_err(sql::sql_error)?;
        let Some(row) = rows.next().map_err(sql::sql_error)? else {
            return Err(unavailable());
        };
        self.reserve_refresh_row(row, 1)?;
        let sql = utf8(text_bytes(row, 0)?)?;
        if sql.len() > 4096
            || sql
                .chars()
                .filter(|c| !c.is_whitespace())
                .flat_map(char::to_lowercase)
                .collect::<String>()
                != "createtableidentity_claims(record_idtextnotnull,pathtextnotnull,file_hashtextnotnull,kindtext,primarykey(record_id,path))"
            || rows.next().map_err(sql::sql_error)?.is_some()
        {
            return Err(unavailable());
        }
        let mut statement = self.connection.prepare(
            "SELECT name='sqlite_autoindex_identity_claims_1',\"unique\",origin='pk',partial FROM pragma_index_list('identity_claims') WHERE name='sqlite_autoindex_identity_claims_1' LIMIT 2",
        ).map_err(sql::sql_error)?;
        let mut rows = statement.query([]).map_err(sql::sql_error)?;
        let Some(row) = rows.next().map_err(sql::sql_error)? else {
            return Err(unavailable());
        };
        self.reserve_refresh_row(row, 4)?;
        let valid = row.get::<_, bool>(0).map_err(sql::sql_error)?
            && row.get::<_, bool>(1).map_err(sql::sql_error)?
            && row.get::<_, bool>(2).map_err(sql::sql_error)?
            && !row.get::<_, bool>(3).map_err(sql::sql_error)?;
        if !valid || rows.next().map_err(sql::sql_error)?.is_some() {
            return Err(unavailable());
        }
        Ok(())
    }

    pub(crate) fn pending_operation_at_start(&self) -> Option<RecordId> {
        self.operation_authority
            .as_ref()
            .and_then(Authority::active)
            .map(|active| active.change.change_id.clone())
    }

    pub(crate) fn usage(&self) -> QueryReadUsage {
        self.usage.get()
    }

    /// Cached document discovery only. Uses the unique BINARY path index and
    /// reserves borrowed scalar bytes before owning paths. Every replay debits
    /// this same reader's cumulative rows, bytes, VM work and SQL deadline.
    pub(crate) fn embedding_document_paths(
        &self,
        after: Option<&VaultRelativePath>,
        limit: usize,
    ) -> Result<Vec<VaultRelativePath>> {
        self.require_refresh_publication()?;
        if limit == 0 || limit > 128 {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "embedding document path page must contain 1..=128 paths",
            ));
        }
        let cursor = after.map_or("", VaultRelativePath::as_str);
        self.reserve_fact_input(cursor.len())?;
        // Captured content has kind NULL, as does unmanaged Markdown. All
        // canonical graph and operational records are deliberately excluded.
        let mut statement = self
            .connection
            .prepare(
                "SELECT path FROM documents INDEXED BY sqlite_autoindex_documents_1 \
             WHERE path COLLATE BINARY > ?1 AND eligibility='current' \
             AND (kind IS NULL OR kind='page') \
             ORDER BY path COLLATE BINARY LIMIT ?2",
            )
            .map_err(sql::sql_error)?;
        let mut rows = statement
            .query(params![cursor, limit as i64])
            .map_err(sql::sql_error)?;
        let mut paths = Vec::new();
        while let Some(row) = rows.next().map_err(sql::sql_error)? {
            self.reserve_refresh_row(row, 1)?;
            let path = VaultRelativePath::new(utf8(text_bytes(row, 0)?)?)?;
            if path.as_str() <= cursor
                || paths
                    .last()
                    .is_some_and(|last: &VaultRelativePath| last >= &path)
            {
                return Err(corrupt(
                    "embedding document path order differs from its key",
                ));
            }
            paths.push(path);
        }
        Ok(paths)
    }

    /// Production scalar lookup reservation, before constructing owned values.
    /// Cached metadata remains discovery; this grants no canonical authority.
    pub(crate) fn reserve_scalar_row(&self, row: &Row<'_>, columns: usize) -> Result<()> {
        self.reserve_refresh_row(row, columns)
    }

    /// Experimental discovery reserves borrowed scalar columns before owning
    /// them, with the same pinned reader's row and byte limits. It does not
    /// authenticate those columns or establish current corpus membership.
    #[cfg(test)]
    pub(crate) fn reserve_experimental_scalar_row(
        &self,
        row: &Row<'_>,
        columns: usize,
    ) -> Result<()> {
        self.reserve_scalar_row(row, columns)
    }

    fn reserve(&self, bytes: usize) -> Result<()> {
        let previous = self.usage.get();
        let rows = previous.rows.checked_add(1);
        let total = previous.bytes.checked_add(bytes);
        if bytes > self.limits.max_row_bytes
            || rows.is_none_or(|rows| rows > self.limits.max_rows)
            || total.is_none_or(|total| total > self.limits.max_bytes)
        {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "catalog selected-row read budget exhausted",
            ));
        }
        // Malformed selected rows still consume budget. Failed reservations do
        // not allocate, deserialize, or refund already attempted decoding work.
        self.usage.set(QueryReadUsage {
            rows: rows.unwrap(),
            bytes: total.unwrap(),
        });
        Ok(())
    }

    fn decode<T: DeserializeOwned>(&self, row: &Row<'_>, column: usize) -> Result<T> {
        let bytes = text_bytes(row, column)?;
        self.reserve(bytes.len())?;
        #[cfg(test)]
        super::query_diagnostics::row(row, row.as_ref().column_count());
        serde_json::from_slice(bytes).map_err(|error| corrupt(error.to_string()))
    }
}

// Refresh discovery stays on the same pinned published epoch as retrieval.
// These helpers do not inspect canonical files or claim fresh global identity.
impl QuerySnapshot {
    fn require_refresh_publication(&self) -> Result<()> {
        let QueryConnection::Normalized(selected) = &self.connection else {
            return Err(WikiError::new(
                ErrorCode::OfflineUnavailable,
                "source refresh requires a normalized published catalog",
            ));
        };
        if !self.snapshot.publication().is_some_and(|binding| {
            binding.file_id == selected.selection().file_id && binding.version == 1
        }) {
            return Err(corrupt("source refresh requires a published epoch binding"));
        }
        Ok(())
    }

    /// Reserve all returned text bytes (including malformed blobs) and the row
    /// before constructing any owned IDs, paths, hashes or record payloads.
    pub(super) fn reserve_refresh_row(&self, row: &Row<'_>, columns: usize) -> Result<()> {
        let mut bytes = 0usize;
        for column in 0..columns {
            let size = match row.get_ref(column).map_err(sql::sql_error)? {
                ValueRef::Text(bytes) | ValueRef::Blob(bytes) => bytes.len(),
                _ => 0,
            };
            bytes = bytes.checked_add(size).ok_or_else(|| {
                WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "refresh lookup byte count overflow",
                )
            })?;
        }
        self.reserve(bytes)?;
        #[cfg(test)]
        super::query_diagnostics::row(row, columns);
        Ok(())
    }
}

// Bounded facts drive the selected overlay projector; they confer no global proof.
impl QuerySnapshot {
    pub(crate) fn require_fact_layout(&self) -> Result<()> {
        self.require_refresh_publication()?;
        let mut statement = self
            .connection
            .prepare("SELECT proof_layout_version FROM catalog_meta WHERE singleton=1")
            .map_err(sql::sql_error)?;
        let mut rows = statement.query([]).map_err(sql::sql_error)?;
        let row = rows
            .next()
            .map_err(sql::sql_error)?
            .ok_or_else(|| corrupt("normalized proof layout header absent"))?;
        self.reserve_refresh_row(row, 1)?;
        match row.get::<_, i64>(0).map_err(sql::sql_error)? {
            2 => Ok(()),
            0 | 1 => Err(WikiError::new(
                ErrorCode::OfflineUnavailable,
                "normalized proof layout requires explicit rebuild",
            )),
            _ => Err(corrupt("unsupported normalized proof layout version")),
        }
    }

    pub(super) fn reserve_fact_input(&self, bytes: usize) -> Result<()> {
        let mut usage = self.usage.get();
        let total = usage
            .bytes
            .checked_add(bytes)
            .filter(|n| *n <= self.limits.max_bytes);
        if bytes > self.limits.max_row_bytes || total.is_none() {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "fact lookup input byte budget exhausted",
            ));
        }
        usage.bytes = total.unwrap();
        self.usage.set(usage);
        Ok(())
    }
    fn fact_requests(&self, count: usize, nonempty: bool) -> Result<()> {
        if count > 4096 || (nonempty && count == 0) {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "fact lookup requires a finite explicit key or role scope",
            ));
        }
        Ok(())
    }
    pub(crate) fn eligibility_fact(
        &self,
        id: &RecordId,
    ) -> Result<Option<super::eligibility_facts::EligibilityFact>> {
        use super::{
            eligibility_facts::{EligibilityBaseline, EligibilityFact},
            structural_rules::StructuralFact,
        };
        self.require_fact_layout()?;
        let mut statement = self.connection.prepare("SELECT r.id,f.baseline_json,r.path,f.structural_json FROM records r INDEXED BY sqlite_autoindex_records_1 LEFT JOIN record_eligibility_facts f ON f.record_id=r.id WHERE r.id=?1").map_err(sql::sql_error)?;
        let mut rows = statement.query([id.as_str()]).map_err(sql::sql_error)?;
        let Some(row) = rows.next().map_err(sql::sql_error)? else {
            return Ok(None);
        };
        self.reserve_refresh_row(row, 4)?;
        if utf8(text_bytes(row, 0)?)? != id.as_str() {
            return Err(corrupt("eligibility fact ID differs from requested record"));
        }
        let baseline: EligibilityBaseline =
            serde_json::from_slice(text_bytes(row, 1)?).map_err(|e| corrupt(e.to_string()))?;
        let own_path =
            VaultRelativePath::new(utf8(text_bytes(row, 2)?)?).map_err(|e| corrupt(e.message))?;
        let structural: StructuralFact =
            serde_json::from_slice(text_bytes(row, 3)?).map_err(|e| corrupt(e.to_string()))?;
        drop(rows);
        drop(statement);
        let mut statement = self.connection.prepare("SELECT owner_id,path FROM record_direct_paths INDEXED BY sqlite_autoindex_record_direct_paths_1 WHERE owner_id=?1 ORDER BY path").map_err(sql::sql_error)?;
        let mut rows = statement.query([id.as_str()]).map_err(sql::sql_error)?;
        let mut direct_paths = BTreeSet::new();
        while let Some(row) = rows.next().map_err(sql::sql_error)? {
            self.reserve_refresh_row(row, 2)?;
            if utf8(text_bytes(row, 0)?)? != id.as_str() {
                return Err(corrupt("direct path owner differs from requested record"));
            }
            direct_paths.insert(
                VaultRelativePath::new(utf8(text_bytes(row, 1)?)?)
                    .map_err(|e| corrupt(e.message))?,
            );
        }
        if !direct_paths.contains(&own_path) {
            return Err(corrupt("eligibility fact omits its own canonical path"));
        }
        Ok(Some(EligibilityFact {
            baseline,
            direct_paths,
            structural,
        }))
    }
    pub(crate) fn direct_path_states(
        &self,
        paths: &[VaultRelativePath],
    ) -> Result<Vec<crate::changes::ReadDependency>> {
        use crate::{changes::ReadDependency, vault::ExpectedState};
        self.fact_requests(paths.len(), false)?;
        self.require_fact_layout()?;
        let mut statement = self.connection.prepare("SELECT path,expected_hash FROM dependencies INDEXED BY sqlite_autoindex_dependencies_1 WHERE path=?1").map_err(sql::sql_error)?;
        let mut result = std::collections::BTreeMap::new();
        for path in paths {
            self.reserve_fact_input(path.as_str().len())?;
            let mut rows = statement.query([path.as_str()]).map_err(sql::sql_error)?;
            let row = rows
                .next()
                .map_err(sql::sql_error)?
                .ok_or_else(|| corrupt("named direct dependency state absent"))?;
            self.reserve_refresh_row(row, 2)?;
            if utf8(text_bytes(row, 0)?)? != path.as_str() {
                return Err(corrupt("direct dependency key differs from requested path"));
            }
            let expected = match row.get_ref(1).map_err(sql::sql_error)? {
                ValueRef::Null => ExpectedState::Absent,
                ValueRef::Text(bytes) => ExpectedState::Hash(
                    Blake3Hash::new(utf8(bytes)?).map_err(|e| corrupt(e.message))?,
                ),
                _ => return Err(corrupt("direct dependency hash has invalid SQL type")),
            };
            result.insert(
                path.clone(),
                ReadDependency {
                    path: path.clone(),
                    expected,
                },
            );
        }
        Ok(result.into_values().collect())
    }
    pub(crate) fn opposition_members(
        &self,
        key: &super::eligibility::OppositionKey,
    ) -> Result<Vec<(RecordId, bool)>> {
        self.require_fact_layout()?;
        self.reserve_fact_input(fact_json_size(key, self.limits.max_row_bytes)?)?;
        let encoded = sql::json(key)?;
        let mut statement = self.connection.prepare("SELECT key_json,negated,assertion_id FROM opposition_members INDEXED BY sqlite_autoindex_opposition_members_1 WHERE key_json=?1 ORDER BY negated,assertion_id").map_err(sql::sql_error)?;
        let mut rows = statement.query([&encoded]).map_err(sql::sql_error)?;
        let mut result = Vec::new();
        while let Some(row) = rows.next().map_err(sql::sql_error)? {
            self.reserve_refresh_row(row, 3)?;
            if utf8(text_bytes(row, 0)?)? != encoded {
                return Err(corrupt(
                    "selected opposition key differs from requested proposition",
                ));
            }
            let negated = match row.get::<_, i64>(1).map_err(sql::sql_error)? {
                0 => false,
                1 => true,
                _ => return Err(corrupt("opposition polarity must be zero or one")),
            };
            let id = RecordId::new(utf8(text_bytes(row, 2)?)?).map_err(|e| corrupt(e.message))?;
            result.push((id, negated));
        }
        Ok(result)
    }
    pub(crate) fn owned_link_facts(
        &self,
        path: &VaultRelativePath,
    ) -> Result<Vec<super::link_facts::OwnedLinkFact>> {
        self.require_fact_layout()?;
        let mut statement = self.connection.prepare("SELECT from_path,byte_start FROM link_facts INDEXED BY sqlite_autoindex_link_facts_1 WHERE from_path=?1 ORDER BY byte_start").map_err(sql::sql_error)?;
        let mut rows = statement.query([path.as_str()]).map_err(sql::sql_error)?;
        let mut result = Vec::new();
        while let Some(row) = rows.next().map_err(sql::sql_error)? {
            self.reserve_refresh_row(row, 2)?;
            if utf8(text_bytes(row, 0)?)? != path.as_str() {
                return Err(corrupt("owned link location differs from requested owner"));
            }
            let offset = u64::try_from(row.get::<_, i64>(1).map_err(sql::sql_error)?)
                .map_err(|_| corrupt("owned link has negative byte offset"))?;
            let fact = self
                .link_fact(path, offset)?
                .ok_or_else(|| corrupt("owned link fact disappeared from pinned publication"))?;
            result.push(fact);
        }
        Ok(result)
    }
    /// Scalar byte reservations for explicit complete-original input, without
    /// decoding cached document text or metadata into owned host values.
    pub(crate) fn original_document_sizes(
        &self,
        path: &VaultRelativePath,
    ) -> Result<Option<(usize, usize)>> {
        self.require_fact_layout()?;
        let payload_bytes = normalized_schema::DOCUMENT_COLUMNS
            .split(',')
            .map(|column| format!("coalesce(length(CAST({column} AS BLOB)),0)"))
            .collect::<Vec<_>>()
            .join("+");
        let mut statement = self.connection.prepare(&format!(
            "SELECT typeof(raw_text),length(CAST(raw_text AS BLOB)),{payload_bytes} FROM documents INDEXED BY sqlite_autoindex_documents_1 WHERE path=?1"
        )).map_err(sql::sql_error)?;
        let mut rows = statement.query([path.as_str()]).map_err(sql::sql_error)?;
        let Some(row) = rows.next().map_err(sql::sql_error)? else {
            return Ok(None);
        };
        self.reserve_refresh_row(row, 3)?;
        if utf8(text_bytes(row, 0)?)? != "text" {
            return Err(corrupt("original input raw text is not SQLite TEXT"));
        }
        let size = |column| -> Result<usize> {
            usize::try_from(row.get::<_, i64>(column).map_err(sql::sql_error)?)
                .map_err(|_| corrupt("original input has invalid indexed byte length"))
        };
        let raw = size(1)?;
        let payload = size(2)?;
        if payload < raw {
            return Err(corrupt("original input payload length is smaller than raw text"));
        }
        Ok(Some((raw, payload)))
    }

    pub(crate) fn document_metadata(
        &self,
        path: &VaultRelativePath,
    ) -> Result<Option<DocumentMetadata>> {
        self.require_fact_layout()?;
        let mut statement = self.connection.prepare("SELECT path,record_id,kind,source_id,owner_revision,eligibility,reasons_json FROM documents INDEXED BY sqlite_autoindex_documents_1 WHERE path=?1").map_err(sql::sql_error)?;
        let mut rows = statement.query([path.as_str()]).map_err(sql::sql_error)?;
        let Some(row) = rows.next().map_err(sql::sql_error)? else {
            return Ok(None);
        };
        self.reserve_refresh_row(row, 7)?;
        if utf8(text_bytes(row, 0)?)? != path.as_str() {
            return Err(corrupt("document metadata differs from requested path"));
        }
        let optional = |column| -> Result<Option<&str>> {
            match row.get_ref(column).map_err(sql::sql_error)? {
                ValueRef::Null => Ok(None),
                ValueRef::Text(bytes) => utf8(bytes).map(Some),
                _ => Err(corrupt(
                    "document metadata optional field is not text or NULL",
                )),
            }
        };
        let record_id = optional(1)?
            .map(RecordId::new)
            .transpose()
            .map_err(|e| corrupt(e.message))?;
        let kind = optional(2)?
            .map(str::parse::<RecordKind>)
            .transpose()
            .map_err(|_| corrupt("document metadata kind is invalid"))?;
        let source_id = optional(3)?
            .map(RecordId::new)
            .transpose()
            .map_err(|e| corrupt(e.message))?;
        let owner_revision = optional(4)?
            .map(RecordId::new)
            .transpose()
            .map_err(|e| corrupt(e.message))?;
        if source_id.is_some() != owner_revision.is_some()
            || (record_id.is_some() && kind.is_none())
            || (owner_revision.is_some() && (record_id.is_some() || kind.is_some()))
        {
            return Err(corrupt(
                "document metadata identity or captured ownership is inconsistent",
            ));
        }
        let eligibility = match utf8(text_bytes(row, 5)?)? {
            "current" => Eligibility::Current,
            "historical" => Eligibility::Historical,
            "stale" => Eligibility::Stale,
            "invalid" => Eligibility::Invalid,
            "withdrawn" => Eligibility::Withdrawn,
            "unsupported" => Eligibility::Unsupported,
            _ => return Err(corrupt("document metadata eligibility is invalid")),
        };
        let reasons =
            serde_json::from_slice(text_bytes(row, 6)?).map_err(|e| corrupt(e.to_string()))?;
        Ok(Some(DocumentMetadata {
            path: path.clone(),
            record_id,
            kind,
            source_id,
            owner_revision,
            eligibility,
            reasons,
        }))
    }
    pub(crate) fn outgoing_edges(
        &self,
        owner: &RecordId,
        roles: &[super::eligibility_facts::EligibilityRole],
    ) -> Result<Vec<super::eligibility_facts::EligibilityEdge>> {
        self.fact_edges(owner, roles, false)
    }
    pub(crate) fn dependent_edges(
        &self,
        target: &RecordId,
        roles: &[super::eligibility_facts::EligibilityRole],
    ) -> Result<Vec<super::eligibility_facts::EligibilityEdge>> {
        self.fact_edges(target, roles, true)
    }
    fn fact_edges(
        &self,
        id: &RecordId,
        roles: &[super::eligibility_facts::EligibilityRole],
        reverse: bool,
    ) -> Result<Vec<super::eligibility_facts::EligibilityEdge>> {
        use super::eligibility_facts::{EligibilityEdge, EligibilityRole};
        self.fact_requests(roles.len(), true)?;
        self.require_fact_layout()?;
        let query = if reverse {
            "SELECT owner_id,target_id,role_json FROM semantic_edges INDEXED BY semantic_dependents WHERE target_id=?1 AND role_json=?2 ORDER BY owner_id"
        } else {
            "SELECT owner_id,target_id,role_json FROM semantic_edges INDEXED BY semantic_outgoing WHERE owner_id=?1 AND role_json=?2 ORDER BY target_id"
        };
        let mut statement = self.connection.prepare(query).map_err(sql::sql_error)?;
        let mut result = BTreeSet::new();
        for requested in roles {
            let bytes = fact_json_size(requested, self.limits.max_row_bytes)?;
            self.reserve_fact_input(bytes)?;
            let encoded = sql::json(requested)?;
            let mut rows = statement
                .query(params![id.as_str(), encoded])
                .map_err(sql::sql_error)?;
            while let Some(row) = rows.next().map_err(sql::sql_error)? {
                self.reserve_refresh_row(row, 3)?;
                let owner_id =
                    RecordId::new(utf8(text_bytes(row, 0)?)?).map_err(|e| corrupt(e.message))?;
                let target_id =
                    RecordId::new(utf8(text_bytes(row, 1)?)?).map_err(|e| corrupt(e.message))?;
                let role: EligibilityRole = serde_json::from_slice(text_bytes(row, 2)?)
                    .map_err(|e| corrupt(e.to_string()))?;
                if &role != requested || (if reverse { &target_id } else { &owner_id }) != id {
                    return Err(corrupt(
                        "selected semantic edge differs from requested scope",
                    ));
                }
                result.insert(EligibilityEdge {
                    owner_id,
                    target_id,
                    role,
                });
            }
        }
        Ok(result.into_iter().collect())
    }
    pub(crate) fn link_fact(
        &self,
        path: &VaultRelativePath,
        byte_start: u64,
    ) -> Result<Option<super::link_facts::OwnedLinkFact>> {
        use super::link_facts::{MatchKey, OwnedLinkFact, TypedLinkTarget};
        self.require_fact_layout()?;
        let offset = sql::integer(byte_start)?;
        let mut statement = self.connection.prepare("SELECT from_path,byte_start,raw_destination,typed_id,typed_kind FROM link_facts INDEXED BY sqlite_autoindex_link_facts_1 WHERE from_path=?1 AND byte_start=?2").map_err(sql::sql_error)?;
        let mut rows = statement
            .query(params![path.as_str(), offset])
            .map_err(sql::sql_error)?;
        let Some(row) = rows.next().map_err(sql::sql_error)? else {
            return Ok(None);
        };
        self.reserve_refresh_row(row, 5)?;
        if utf8(text_bytes(row, 0)?)? != path.as_str()
            || row.get::<_, i64>(1).map_err(sql::sql_error)? != offset
        {
            return Err(corrupt(
                "selected link fact differs from requested location",
            ));
        }
        let raw_destination = utf8(text_bytes(row, 2)?)?.to_owned();
        let typed = match (
            row.get_ref(3).map_err(sql::sql_error)?,
            row.get_ref(4).map_err(sql::sql_error)?,
        ) {
            (ValueRef::Null, ValueRef::Null) => None,
            (ValueRef::Text(id), ValueRef::Text(kind)) => Some(TypedLinkTarget {
                id: RecordId::new(utf8(id)?).map_err(|e| corrupt(e.message))?,
                expected_kind: utf8(kind)?
                    .parse()
                    .map_err(|_| corrupt("invalid typed companion kind"))?,
            }),
            _ => return Err(corrupt("link fact typed identity is incomplete")),
        };
        drop(rows);
        drop(statement);
        let mut statement = self.connection.prepare("SELECT kind,value FROM link_match_keys INDEXED BY link_match_owners WHERE from_path=?1 AND byte_start=?2").map_err(sql::sql_error)?;
        let mut rows = statement
            .query(params![path.as_str(), offset])
            .map_err(sql::sql_error)?;
        let mut keys = BTreeSet::new();
        while let Some(row) = rows.next().map_err(sql::sql_error)? {
            self.reserve_refresh_row(row, 2)?;
            keys.insert(MatchKey {
                kind: fact_key_kind(utf8(text_bytes(row, 0)?)?)?,
                value: utf8(text_bytes(row, 1)?)?.to_owned(),
            });
        }
        if let Some(target) = &typed {
            let expected = super::link_facts::typed_fact(
                path,
                byte_start,
                &raw_destination,
                &target.id,
                target.expected_kind,
            )?;
            if keys.iter().ne(expected.keys.iter()) {
                return Err(corrupt(
                    "typed link keys differ from exact companion semantics",
                ));
            }
        } else {
            let resolution = match crate::records::links::untyped_lookup(&raw_destination) {
                crate::records::links::UntypedLookup::External => {
                    crate::records::LinkResolution::External
                }
                _ => crate::records::LinkResolution::Missing,
            };
            let expected =
                super::link_facts::untyped_fact(path, byte_start, &raw_destination, &resolution)?;
            let static_keys = keys
                .iter()
                .filter(|key| key.kind != super::link_facts::MatchKeyKind::Id);
            if static_keys.ne(expected.keys.iter()) {
                return Err(corrupt(
                    "untyped link keys differ from raw destination semantics",
                ));
            }
            let mut ids = keys
                .iter()
                .filter(|key| key.kind == super::link_facts::MatchKeyKind::Id);
            if let Some(key) = ids.next() {
                RecordId::new(&key.value).map_err(|e| corrupt(e.message))?;
                if ids.next().is_some() {
                    return Err(corrupt("untyped link stores more than one resolved ID"));
                }
            }
        }
        Ok(Some(OwnedLinkFact {
            from_path: path.clone(),
            byte_start,
            raw_destination,
            typed,
            keys: keys.into_iter().collect(),
        }))
    }
    pub(crate) fn affected_links(
        &self,
        keys: &[super::link_facts::MatchKey],
    ) -> Result<Vec<(VaultRelativePath, u64)>> {
        self.fact_requests(keys.len(), false)?;
        self.require_fact_layout()?;
        let mut statement = self.connection.prepare("SELECT kind,value,from_path,byte_start FROM link_match_keys INDEXED BY sqlite_autoindex_link_match_keys_1 WHERE kind=?1 AND value=?2 ORDER BY from_path,byte_start").map_err(sql::sql_error)?;
        let mut result = BTreeSet::new();
        for key in keys {
            self.reserve_fact_input(key.value.len())?;
            let mut rows = statement
                .query(params![fact_key_name(key.kind), key.value])
                .map_err(sql::sql_error)?;
            while let Some(row) = rows.next().map_err(sql::sql_error)? {
                self.reserve_refresh_row(row, 4)?;
                if fact_key_kind(utf8(text_bytes(row, 0)?)?)? != key.kind
                    || utf8(text_bytes(row, 1)?)? != key.value
                {
                    return Err(corrupt(
                        "selected reverse link key differs from requested key",
                    ));
                }
                let offset = u64::try_from(row.get::<_, i64>(3).map_err(sql::sql_error)?)
                    .map_err(|_| corrupt("negative reverse link byte offset"))?;
                let path = VaultRelativePath::new(utf8(text_bytes(row, 2)?)?)
                    .map_err(|e| corrupt(e.message))?;
                result.insert((path, offset));
            }
        }
        Ok(result.into_iter().collect())
    }
    pub(crate) fn registry_candidates_for_key(
        &self,
        key: &super::link_facts::MatchKey,
    ) -> Result<Vec<crate::records::RegistryEntry>> {
        use crate::records::RegistryEntry;
        self.require_fact_layout()?;
        self.reserve_fact_input(key.value.len())?;
        let mut statement = self.connection.prepare("SELECT kind,value,record_id,path FROM registry_match_keys INDEXED BY sqlite_autoindex_registry_match_keys_1 WHERE kind=?1 AND value=?2 ORDER BY record_id,path").map_err(sql::sql_error)?;
        let mut rows = statement
            .query(params![fact_key_name(key.kind), key.value])
            .map_err(sql::sql_error)?;
        let mut result = Vec::new();
        while let Some(row) = rows.next().map_err(sql::sql_error)? {
            self.reserve_refresh_row(row, 4)?;
            if fact_key_kind(utf8(text_bytes(row, 0)?)?)? != key.kind
                || utf8(text_bytes(row, 1)?)? != key.value
            {
                return Err(corrupt("selected registry key differs from requested key"));
            }
            let id = RecordId::new(utf8(text_bytes(row, 2)?)?).map_err(|e| corrupt(e.message))?;
            let path = VaultRelativePath::new(utf8(text_bytes(row, 3)?)?)
                .map_err(|e| corrupt(e.message))?;
            let adopted = QueryCatalog::record(self, &id)?
                .ok_or_else(|| corrupt("registry match has no adopted record"))?;
            if adopted.path != path {
                return Err(corrupt(
                    "registry candidate path differs from adopted record",
                ));
            }
            let entry = RegistryEntry {
                id,
                kind: adopted.record.kind(),
                path,
                aliases: scan::list(&adopted.record, "aliases"),
            };
            if !super::link_facts::registry_keys(&entry)?.contains(key) {
                return Err(corrupt("registry match key differs from adopted record"));
            }
            result.push(entry);
        }
        Ok(result)
    }
    /// Complete saturated cardinality, rather than a truncated candidate list.
    /// Two distinct adopted entry witnesses suffice to prove ambiguity.
    pub(crate) fn registry_probe_for_key(
        &self,
        key: &super::link_facts::MatchKey,
    ) -> Result<super::navigation_resolution::RegistryProbe> {
        self.registry_probe_for_key_excluding(key, &BTreeSet::new())
    }
    /// Probe the complete surviving bucket after a bounded owner replacement.
    /// Exclusions must be applied before saturation, otherwise hidden survivors
    /// beyond the first two witnesses can be lost.
    pub(crate) fn registry_probe_for_key_excluding(
        &self,
        key: &super::link_facts::MatchKey,
        excluded_paths: &BTreeSet<VaultRelativePath>,
    ) -> Result<super::navigation_resolution::RegistryProbe> {
        use super::{
            link_facts::MatchKeyKind,
            navigation_resolution::{RegistryCandidate, RegistryProbe},
        };
        if excluded_paths.len() > 16 {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "registry replacement excludes more than 16 owners",
            ));
        }
        self.require_fact_layout()?;
        self.reserve_fact_input(key.value.len())?;
        for path in excluded_paths {
            self.reserve_fact_input(path.as_str().len())?;
        }
        let query = registry_probe_sql(excluded_paths.len());
        let mut statement = self.connection.prepare(&query).map_err(sql::sql_error)?;
        let inputs = std::iter::once(fact_key_name(key.kind))
            .chain(std::iter::once(key.value.as_str()))
            .chain(excluded_paths.iter().map(VaultRelativePath::as_str));
        let mut rows = statement
            .query(rusqlite::params_from_iter(inputs))
            .map_err(sql::sql_error)?;
        let mut result = RegistryProbe::Zero;
        while let Some(row) = rows.next().map_err(sql::sql_error)? {
            self.reserve_refresh_row(row, 7)?;
            if fact_key_kind(utf8(text_bytes(row, 0)?)?)? != key.kind
                || utf8(text_bytes(row, 1)?)? != key.value
                || text_bytes(row, 2)? != text_bytes(row, 4)?
                || text_bytes(row, 3)? != text_bytes(row, 6)?
            {
                return Err(corrupt("registry witness differs from its adopted record"));
            }
            let id = RecordId::new(utf8(text_bytes(row, 2)?)?).map_err(|e| corrupt(e.message))?;
            let path = VaultRelativePath::new(utf8(text_bytes(row, 3)?)?)
                .map_err(|e| corrupt(e.message))?;
            let kind = utf8(text_bytes(row, 5)?)?
                .parse()
                .map_err(|e: WikiError| corrupt(e.message))?;
            let valid = match key.kind {
                MatchKeyKind::Id => key.value == id.as_str(),
                MatchKeyKind::Path => key.value == path.as_str(),
                MatchKeyKind::Basename => {
                    key.value == crate::records::links::registry_basename(path.as_str())
                }
                MatchKeyKind::Alias => {
                    // Alias membership lives in canonical metadata. Decode at
                    // most two selected records, never the entire alias bucket.
                    let adopted = QueryCatalog::record(self, &id)?
                        .ok_or_else(|| corrupt("alias witness lacks adopted record"))?;
                    if adopted.path != path || adopted.record.kind() != kind {
                        return Err(corrupt("alias witness differs from adopted record"));
                    }
                    scan::list(&adopted.record, "aliases")
                        .iter()
                        .any(|alias| alias == &key.value)
                }
            };
            if !valid {
                return Err(corrupt("registry witness key differs from adopted record"));
            }
            result.insert(RegistryCandidate { id, kind, path })?;
        }
        Ok(result)
    }
}
fn decode_identity_claim(row: &Row<'_>, id: &RecordId) -> Result<super::IdentityClaimRow> {
    let claimed_id = RecordId::new(utf8(text_bytes(row, 0)?)?).map_err(|e| corrupt(e.message))?;
    let path =
        VaultRelativePath::new(utf8(text_bytes(row, 1)?)?).map_err(|e| corrupt(e.message))?;
    let hash = Blake3Hash::new(utf8(text_bytes(row, 2)?)?).map_err(|e| corrupt(e.message))?;
    let kind = match row.get_ref(3).map_err(sql::sql_error)? {
        ValueRef::Null => None,
        ValueRef::Text(bytes) => Some(
            utf8(bytes)?
                .parse::<RecordKind>()
                .map_err(|_| corrupt("identity claim kind is invalid"))?,
        ),
        _ => return Err(corrupt("identity claim kind is not text or NULL")),
    };
    if &claimed_id != id {
        return Err(corrupt("identity claim key differs from requested ID"));
    }
    Ok(super::IdentityClaimRow {
        id: claimed_id,
        path,
        hash,
        kind,
    })
}

fn registry_probe_sql(exclusions: usize) -> String {
    let predicate = if exclusions == 0 {
        String::new()
    } else {
        format!(
            " AND k.path NOT IN ({})",
            (0..exclusions).map(|_| "?").collect::<Vec<_>>().join(",")
        )
    };
    format!(
        "SELECT k.kind,k.value,k.record_id,k.path,r.id,r.kind,r.path FROM registry_match_keys k INDEXED BY sqlite_autoindex_registry_match_keys_1 LEFT JOIN records r INDEXED BY sqlite_autoindex_records_1 ON r.id=k.record_id WHERE k.kind=? AND k.value=?{predicate} ORDER BY k.record_id,k.path LIMIT 2"
    )
}
fn fact_key_name(kind: super::link_facts::MatchKeyKind) -> &'static str {
    use super::link_facts::MatchKeyKind;
    match kind {
        MatchKeyKind::Id => "id",
        MatchKeyKind::Path => "path",
        MatchKeyKind::Basename => "basename",
        MatchKeyKind::Alias => "alias",
    }
}
fn fact_key_kind(value: &str) -> Result<super::link_facts::MatchKeyKind> {
    use super::link_facts::MatchKeyKind;
    match value {
        "id" => Ok(MatchKeyKind::Id),
        "path" => Ok(MatchKeyKind::Path),
        "basename" => Ok(MatchKeyKind::Basename),
        "alias" => Ok(MatchKeyKind::Alias),
        _ => Err(corrupt("unknown normalized match key kind")),
    }
}
fn fact_json_size(value: &impl serde::Serialize, limit: usize) -> Result<usize> {
    struct Counter {
        bytes: usize,
        limit: usize,
    }
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes = self
                .bytes
                .checked_add(bytes.len())
                .filter(|n| *n <= self.limit)
                .ok_or_else(|| std::io::Error::other("fact lookup input exceeds byte ceiling"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter { bytes: 0, limit };
    serde_json::to_writer(&mut counter, value).map_err(|_| {
        WikiError::new(
            ErrorCode::BudgetExceeded,
            "fact lookup input exceeds byte ceiling",
        )
    })?;
    Ok(counter.bytes)
}

impl crate::sources::SourceRefreshLookup for QuerySnapshot {
    fn snapshot(&self) -> &ReadSnapshot {
        &self.snapshot
    }
    fn vault_id(&self) -> &RecordId {
        &self.vault_id
    }

    fn unique_record(&self, id: &RecordId) -> Result<Option<crate::sources::RefreshRecord>> {
        self.require_refresh_publication()?;
        let mut statement = self.connection.prepare(
            "SELECT path,file_hash,kind FROM identity_claims INDEXED BY sqlite_autoindex_identity_claims_1 WHERE record_id=?1 ORDER BY path LIMIT 2"
        ).map_err(sql::sql_error)?;
        let mut rows = statement.query([id.as_str()]).map_err(sql::sql_error)?;
        let Some(row) = rows.next().map_err(sql::sql_error)? else {
            // A missing identity claim must not silently hide an adopted record.
            if QueryCatalog::record(self, id)?.is_some() {
                return Err(corrupt("adopted record has no identity claim"));
            }
            return Ok(None);
        };
        self.reserve_refresh_row(row, 3)?;
        let path = VaultRelativePath::new(utf8(text_bytes(row, 0)?)?)
            .map_err(|error| corrupt(error.message))?;
        let hash =
            Blake3Hash::new(utf8(text_bytes(row, 1)?)?).map_err(|error| corrupt(error.message))?;
        let kind = utf8(text_bytes(row, 2)?)?.to_owned();
        if let Some(row) = rows.next().map_err(sql::sql_error)? {
            self.reserve_refresh_row(row, 3)?;
            return Err(corrupt("record ID has multiple canonical identity claims"));
        }
        let adopted = QueryCatalog::record(self, id)?
            .ok_or_else(|| corrupt("claimed record is malformed or not adopted"))?;
        if adopted.path != path || adopted.hash != hash || adopted.record.kind().as_str() != kind {
            return Err(corrupt("identity claim differs from adopted record"));
        }
        if adopted.eligibility == crate::domain::Eligibility::Invalid
            && matches!(
                adopted.record.kind(),
                crate::domain::RecordKind::Source | crate::domain::RecordKind::Revision
            )
        {
            return Err(WikiError::new(
                ErrorCode::SourceIntegrity,
                "selected source or revision has a known invalid baseline",
            ));
        }
        Ok(Some(crate::sources::RefreshRecord {
            record: adopted.record,
            path: adopted.path,
            hash: adopted.hash,
        }))
    }

    fn record_at_path(
        &self,
        path: &VaultRelativePath,
    ) -> Result<Option<crate::sources::RefreshRecord>> {
        self.require_refresh_publication()?;
        let mut statement = self
            .connection
            .prepare("SELECT id,path FROM records INDEXED BY record_paths WHERE path=?1")
            .map_err(sql::sql_error)?;
        let mut rows = statement.query([path.as_str()]).map_err(sql::sql_error)?;
        let Some(row) = rows.next().map_err(sql::sql_error)? else {
            return Ok(None);
        };
        self.reserve_refresh_row(row, 2)?;
        if utf8(text_bytes(row, 1)?)? != path.as_str() {
            return Err(corrupt("selected record path differs from lookup key"));
        }
        let id =
            RecordId::new(utf8(text_bytes(row, 0)?)?).map_err(|error| corrupt(error.message))?;
        let record = self
            .unique_record(&id)?
            .ok_or_else(|| corrupt("selected path has no uniquely claimed record"))?;
        if &record.path != path {
            return Err(corrupt("selected record path differs from its claim"));
        }
        Ok(Some(record))
    }

    fn id_is_claimed(&self, id: &RecordId) -> Result<bool> {
        self.require_refresh_publication()?;
        let mut statement = self.connection.prepare(
            "SELECT record_id FROM identity_claims INDEXED BY sqlite_autoindex_identity_claims_1 WHERE record_id=?1 UNION ALL SELECT id FROM records INDEXED BY sqlite_autoindex_records_1 WHERE id=?1 LIMIT 1"
        ).map_err(sql::sql_error)?;
        let mut rows = statement.query([id.as_str()]).map_err(sql::sql_error)?;
        let Some(row) = rows.next().map_err(sql::sql_error)? else {
            return Ok(false);
        };
        self.reserve_refresh_row(row, 1)?;
        if utf8(text_bytes(row, 0)?)? != id.as_str() {
            return Err(corrupt("identity claim differs from lookup ID"));
        }
        Ok(true)
    }

    fn revision_identity_is_reserved(
        &self,
        id: &RecordId,
        path: &VaultRelativePath,
    ) -> Result<bool> {
        self.require_policy_layout()?;
        let policy_key =
            sql::json(&crate::graph::policy_inputs::PolicyInputKey::CanonicalIdentity(id.clone()))?;
        let input_bytes = id
            .as_str()
            .len()
            .checked_add(path.as_str().len())
            .ok_or_else(|| {
                WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "revision reservation input byte count overflow",
                )
            })?;
        self.reserve_fact_input(input_bytes)?;
        self.reserve_fact_input(policy_key.len())?;
        // Prepare all indexed probes together so a missing required table
        // or index refuses even when an earlier branch would find a reservation.
        // LIMIT 1 proves existence; it is not a partial candidate collection.
        let mut statement = self.connection.prepare(
            "SELECT 0,record_id,'','' FROM identity_claims INDEXED BY sqlite_autoindex_identity_claims_1 WHERE record_id=?1 \
             UNION ALL SELECT 0,id,'','' FROM records INDEXED BY sqlite_autoindex_records_1 WHERE id=?1 \
             UNION ALL SELECT 1,target_id,'','' FROM semantic_edges INDEXED BY semantic_dependents WHERE target_id=?1 \
             UNION ALL SELECT 2,value,'','' FROM link_match_keys INDEXED BY sqlite_autoindex_link_match_keys_1 WHERE kind='path' AND value=?2 \
             UNION ALL SELECT 3,key,owner,value FROM policy_facts INDEXED BY sqlite_autoindex_policy_facts_1 WHERE family='dependency' AND key=?3 LIMIT 1"
        ).map_err(sql::sql_error)?;
        let mut rows = statement
            .query(params![id.as_str(), path.as_str(), policy_key])
            .map_err(sql::sql_error)?;
        let Some(row) = rows.next().map_err(sql::sql_error)? else {
            return Ok(false);
        };
        self.reserve_refresh_row(row, 4)?;
        let expected = match row.get::<_, i64>(0).map_err(sql::sql_error)? {
            0 | 1 => id.as_str(),
            2 => path.as_str(),
            3 => {
                super::policy_facts::PolicyRow::from_columns([
                    "dependency".into(),
                    utf8(text_bytes(row, 1)?)?.into(),
                    utf8(text_bytes(row, 2)?)?.into(),
                    utf8(text_bytes(row, 3)?)?.into(),
                ])?;
                policy_key.as_str()
            }
            _ => return Err(corrupt("unknown revision reservation witness")),
        };
        if utf8(text_bytes(row, 1)?)? != expected {
            return Err(corrupt(
                "revision reservation witness differs from lookup key",
            ));
        }
        Ok(true)
    }

    fn matching_revision(
        &self,
        source: &RecordId,
        signature: &crate::sources::RevisionSignature,
    ) -> Result<Option<crate::sources::MatchingRevision>> {
        self.require_refresh_publication()?;
        let mut statement = self.connection.prepare(
            "SELECT source_id,revision_id,retained_ordinal,original_hash,content_hash,extractor_fingerprint,extraction_status FROM source_revision_identity INDEXED BY source_revision_matches WHERE source_id=?1 AND original_hash=?2 AND content_hash IS ?3 AND extractor_fingerprint=?4 ORDER BY retained_ordinal LIMIT 1"
        ).map_err(sql::sql_error)?;
        let mut rows = statement
            .query(params![
                source.as_str(),
                signature.original_hash.as_str(),
                signature.content_hash.as_ref().map(Blake3Hash::as_str),
                signature.extractor_fingerprint.as_str()
            ])
            .map_err(sql::sql_error)?;
        let Some(row) = rows.next().map_err(sql::sql_error)? else {
            return Ok(None);
        };
        self.reserve_refresh_row(row, 7)?;
        let optional = |column| -> Result<Option<&str>> {
            match row.get_ref(column).map_err(sql::sql_error)? {
                ValueRef::Null => Ok(None),
                ValueRef::Text(bytes) => utf8(bytes).map(Some),
                _ => Err(corrupt("revision content hash has invalid SQL type")),
            }
        };
        let retained_ordinal = usize::try_from(row.get::<_, i64>(2).map_err(sql::sql_error)?)
            .map_err(|_| corrupt("revision retained ordinal is invalid"))?;
        if utf8(text_bytes(row, 0)?)? != source.as_str()
            || utf8(text_bytes(row, 3)?)? != signature.original_hash.as_str()
            || optional(4)? != signature.content_hash.as_ref().map(Blake3Hash::as_str)
            || utf8(text_bytes(row, 5)?)? != signature.extractor_fingerprint.as_str()
        {
            return Err(corrupt(
                "selected revision tuple differs from lookup signature",
            ));
        }
        let id =
            RecordId::new(utf8(text_bytes(row, 1)?)?).map_err(|error| corrupt(error.message))?;
        let status = utf8(text_bytes(row, 6)?)?;
        let selected = self
            .unique_record(&id)?
            .ok_or_else(|| corrupt("matched revision has no uniquely claimed record"))?;
        let record = &selected.record;
        if record.kind() != crate::domain::RecordKind::Revision
            || record.string("wiki_source_id") != Some(source.as_str())
            || record.string("wiki_original_hash") != Some(signature.original_hash.as_str())
            || record.string("wiki_content_hash")
                != signature.content_hash.as_ref().map(Blake3Hash::as_str)
            || record.string("wiki_extractor_fingerprint")
                != Some(signature.extractor_fingerprint.as_str())
            || record.string("wiki_extraction_status") != Some(status)
        {
            return Err(corrupt(
                "matched revision identity differs from canonical record",
            ));
        }
        Ok(Some(crate::sources::MatchingRevision {
            revision: selected,
            retained_ordinal,
        }))
    }

    fn source_assertions(&self, source: &RecordId) -> Result<Vec<RecordId>> {
        self.require_refresh_publication()?;
        let mut statement = self.connection.prepare(
            "SELECT DISTINCT assertion_id FROM source_evidence INDEXED BY source_assertions WHERE source_id=?1 ORDER BY assertion_id LIMIT ?2"
        ).map_err(sql::sql_error)?;
        let remaining = self.limits.max_rows.saturating_sub(self.usage.get().rows);
        let mut rows = statement
            .query(params![source.as_str(), (remaining + 1) as i64])
            .map_err(sql::sql_error)?;
        let mut result = Vec::new();
        while let Some(row) = rows.next().map_err(sql::sql_error)? {
            self.reserve_refresh_row(row, 1)?;
            result.push(
                RecordId::new(utf8(text_bytes(row, 0)?)?)
                    .map_err(|error| corrupt(error.message))?,
            );
        }
        Ok(result)
    }
}

impl crate::changes::RevisionOwnershipLookup for QuerySnapshot {
    fn snapshot(&self) -> &ReadSnapshot {
        &self.snapshot
    }

    fn vault_id(&self) -> &RecordId {
        &self.vault_id
    }

    fn require_ready(&self) -> Result<()> {
        self.require_refresh_publication()?;
        let mut statement = self
            .connection
            .prepare("SELECT revision_ownership_version FROM catalog_meta WHERE singleton=1")
            .map_err(sql::sql_error)?;
        let mut rows = statement.query([]).map_err(sql::sql_error)?;
        let row = rows
            .next()
            .map_err(sql::sql_error)?
            .ok_or_else(|| corrupt("revision ownership header is absent"))?;
        self.reserve_refresh_row(row, 1)?;
        match row.get::<_, i64>(0).map_err(sql::sql_error)? {
            1 => Ok(()),
            0 => Err(WikiError::new(
                ErrorCode::OfflineUnavailable,
                "revision ownership registry requires explicit reconstruction",
            )),
            _ => Err(corrupt("unsupported revision ownership registry version")),
        }
    }

    fn revision_owner(
        &self,
        key: &crate::changes::RevisionTreeKey,
    ) -> Result<Option<crate::changes::PreparedChange>> {
        self.require_ready()?;
        let mut statement = self.connection.prepare(
            "SELECT source_component,revision_component,change_id,manifest_hash FROM revision_tree_owners INDEXED BY sqlite_autoindex_revision_tree_owners_1 WHERE source_component=?1 AND revision_component=?2"
        ).map_err(sql::sql_error)?;
        let mut rows = statement
            .query(params![key.source_component, key.revision_component])
            .map_err(sql::sql_error)?;
        let Some(row) = rows.next().map_err(sql::sql_error)? else {
            return Ok(None);
        };
        self.reserve_refresh_row(row, 4)?;
        if utf8(text_bytes(row, 0)?)? != key.source_component
            || utf8(text_bytes(row, 1)?)? != key.revision_component
        {
            return Err(corrupt("revision ownership key differs from lookup"));
        }
        Ok(Some(crate::changes::PreparedChange {
            change_id: RecordId::new(utf8(text_bytes(row, 2)?)?)
                .map_err(|error| corrupt(error.message))?,
            manifest_hash: Blake3Hash::new(utf8(text_bytes(row, 3)?)?)
                .map_err(|error| corrupt(error.message))?,
        }))
    }
}

impl QueryCatalog for QuerySnapshot {
    fn check_query_budget(&self) -> Result<()> {
        if self.query_started.elapsed() >= Duration::from_millis(self.limits.max_elapsed_ms) {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "catalog read elapsed budget exhausted; narrow the source or path filters",
            ));
        }
        Ok(())
    }
    fn publication_id(&self) -> Option<&str> {
        match &self.connection {
            QueryConnection::Normalized(selected) => Some(&selected.selection().file_id),
            QueryConnection::Legacy(_) => None,
        }
    }
    fn normalized_layout(&self) -> bool {
        matches!(&self.connection, QueryConnection::Normalized(_))
    }
    fn connection(&self) -> &Connection {
        &self.connection
    }
    fn snapshot(&self) -> &ReadSnapshot {
        &self.snapshot
    }
    fn vault_id(&self) -> &RecordId {
        &self.vault_id
    }
    fn verification(&self) -> &SnapshotVerification {
        &self.verification
    }
    fn record(&self, id: &RecordId) -> Result<Option<RecordRow>> {
        if self.normalized_layout() {
            let mut statement = self.connection.prepare(
                "SELECT row_json,id,kind,path,hash,authored_status,eligibility,identity_eligibility,description_eligibility,disputed FROM records INDEXED BY sqlite_autoindex_records_1 WHERE id=?1",
            ).map_err(sql::sql_error)?;
            let mut rows = statement.query([id.as_str()]).map_err(sql::sql_error)?;
            let Some(row) = rows.next().map_err(sql::sql_error)? else {
                return Ok(None);
            };
            let mut bytes = 0usize;
            for column in 0..9 {
                let length = match row.get_ref(column).map_err(sql::sql_error)? {
                    ValueRef::Text(value) => value.len(),
                    ValueRef::Null if matches!(column, 5 | 7 | 8) => 0,
                    _ => return Err(corrupt("normalized record column has the wrong SQL type")),
                };
                bytes = bytes.checked_add(length).ok_or_else(|| {
                    WikiError::new(
                        ErrorCode::BudgetExceeded,
                        "normalized record byte count overflow",
                    )
                })?;
            }
            self.reserve(bytes)?;
            #[cfg(test)]
            super::query_diagnostics::row(row, 10);
            let decoded: RecordRow = serde_json::from_slice(text_bytes(row, 0)?)
                .map_err(|error| corrupt(error.to_string()))?;
            let eligibility = |value: crate::domain::Eligibility| match value {
                crate::domain::Eligibility::Current => "current",
                crate::domain::Eligibility::Historical => "historical",
                crate::domain::Eligibility::Stale => "stale",
                crate::domain::Eligibility::Withdrawn => "withdrawn",
                crate::domain::Eligibility::Unsupported => "unsupported",
                crate::domain::Eligibility::Invalid => "invalid",
            };
            let optional = |column| -> Result<Option<&str>> {
                match row.get_ref(column).map_err(sql::sql_error)? {
                    ValueRef::Null => Ok(None),
                    ValueRef::Text(bytes) => utf8(bytes).map(Some),
                    _ => Err(corrupt("normalized record optional value is invalid")),
                }
            };
            if decoded.record.id() != id
                || utf8(text_bytes(row, 1)?)? != id.as_str()
                || utf8(text_bytes(row, 2)?)? != decoded.record.kind().as_str()
                || utf8(text_bytes(row, 3)?)? != decoded.path.as_str()
                || utf8(text_bytes(row, 4)?)? != decoded.hash.as_str()
                || optional(5)? != decoded.authored_status.as_deref()
                || utf8(text_bytes(row, 6)?)? != eligibility(decoded.eligibility)
                || optional(7)? != decoded.identity_eligibility.map(eligibility)
                || optional(8)? != decoded.description_eligibility.map(eligibility)
                || row.get::<_, i64>(9).map_err(sql::sql_error)? != i64::from(decoded.disputed)
            {
                return Err(corrupt(
                    "selected record differs from its normalized columns",
                ));
            }
            return Ok(Some(decoded));
        }
        let mut statement = self.connection.prepare(
            "SELECT row_json FROM records INDEXED BY sqlite_autoindex_records_1 WHERE gen=?1 AND id=?2",
        ).map_err(sql::sql_error)?;
        let mut rows = statement
            .query(params![
                sql::integer(self.snapshot.generation)?,
                id.as_str()
            ])
            .map_err(sql::sql_error)?;
        let Some(row) = rows.next().map_err(sql::sql_error)? else {
            return Ok(None);
        };
        let decoded: RecordRow = self.decode(row, 0)?;
        if decoded.record.id() != id {
            return Err(corrupt("selected record ID differs from its indexed key"));
        }
        Ok(Some(decoded))
    }
    fn document(&self, path: &VaultRelativePath) -> Result<Option<DocumentRow>> {
        if self.normalized_layout() {
            let mut statement = self.connection.prepare(&format!(
                "SELECT {} FROM documents INDEXED BY sqlite_autoindex_documents_1 WHERE path=?1",
                normalized_schema::DOCUMENT_COLUMNS,
            )).map_err(sql::sql_error)?;
            let mut rows = statement.query([path.as_str()]).map_err(sql::sql_error)?;
            let Some(row) = rows.next().map_err(sql::sql_error)? else {
                return Ok(None);
            };
            let decoded = self.decode_document(row, 0)?;
            if &decoded.path != path {
                return Err(corrupt(
                    "selected document path differs from its indexed key",
                ));
            }
            return Ok(Some(decoded));
        }
        let mut statement = self.connection.prepare(
            "SELECT row_json FROM documents INDEXED BY sqlite_autoindex_documents_1 WHERE gen=?1 AND path=?2",
        ).map_err(sql::sql_error)?;
        let mut rows = statement
            .query(params![
                sql::integer(self.snapshot.generation)?,
                path.as_str()
            ])
            .map_err(sql::sql_error)?;
        let Some(row) = rows.next().map_err(sql::sql_error)? else {
            return Ok(None);
        };
        let decoded = self.decode_document(row, 0)?;
        if &decoded.path != path {
            return Err(corrupt(
                "selected document path differs from its indexed key",
            ));
        }
        Ok(Some(decoded))
    }
    fn diagnostics(&self, paths: &BTreeSet<VaultRelativePath>) -> Result<Vec<CatalogDiagnostic>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        if paths.len() > self.limits.max_rows {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "catalog diagnostic path budget exhausted",
            ));
        }
        if self.normalized_layout() {
            // Column names alone admit a partial index that silently excludes
            // selected warnings, or a collation that changes path equality.
            let mut statement = self.connection.prepare(
                "SELECT sql FROM sqlite_schema WHERE type='index' AND name='diagnostic_paths' AND tbl_name='diagnostics' LIMIT 2",
            ).map_err(sql::sql_error)?;
            let mut rows = statement.query([]).map_err(sql::sql_error)?;
            let valid = if let Some(row) = rows.next().map_err(sql::sql_error)? {
                let bytes = text_bytes(row, 0)?;
                bytes.len() <= 4096
                    && utf8(bytes)?
                        .trim()
                        .trim_end_matches(';')
                        .chars()
                        .filter(|character| !character.is_whitespace())
                        .map(|character| character.to_ascii_lowercase())
                        .collect::<String>()
                        == "createindexdiagnostic_pathsondiagnostics(path)"
            } else {
                false
            };
            if !valid || rows.next().map_err(sql::sql_error)?.is_some() {
                let mut error = WikiError::new(
                    ErrorCode::CapabilityUnavailable,
                    "catalog lacks the bounded diagnostic path index",
                );
                error.hint = Some("run index rebuild".into());
                return Err(error);
            }
        }
        // Old schema-v1 caches may lack this additional access index. Do not
        // silently replace selected lookups with a complete diagnostic scan.
        let mut index = self.connection.prepare(
            "SELECT seqno,name='gen',name='path' FROM pragma_index_info('diagnostic_paths') ORDER BY seqno LIMIT 3",
        ).map_err(sql::sql_error)?;
        let columns = index
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<bool>>(1)?,
                    row.get::<_, Option<bool>>(2)?,
                ))
            })
            .map_err(sql::sql_error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(sql::sql_error)?;
        let expected = if self.normalized_layout() {
            vec![(0, Some(false), Some(true))]
        } else {
            vec![(0, Some(true), Some(false)), (1, Some(false), Some(true))]
        };
        if columns != expected {
            let mut error = WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "catalog lacks the bounded diagnostic path index",
            );
            error.hint = Some("run index rebuild".into());
            return Err(error);
        }
        let query = if self.normalized_layout() {
            "SELECT path,record_id,code,details_json FROM diagnostics INDEXED BY diagnostic_paths WHERE path=?2 ORDER BY diagnostic_row LIMIT ?3"
        } else {
            "SELECT path,record_id,code,details_json FROM diagnostics INDEXED BY diagnostic_paths WHERE gen=?1 AND path=?2 ORDER BY rowid LIMIT ?3"
        };
        let mut statement = self.connection.prepare(query).map_err(sql::sql_error)?;
        let mut result = Vec::new();
        for path in paths {
            // Read one excess row to fail rather than silently truncate diagnostics.
            let remaining = self.limits.max_rows.saturating_sub(self.usage.get().rows);
            let mut rows = statement
                .query(params![
                    sql::integer(self.snapshot.generation)?,
                    path.as_str(),
                    (remaining + 1) as i64,
                ])
                .map_err(sql::sql_error)?;
            while let Some(row) = rows.next().map_err(sql::sql_error)? {
                let raw_path = text_bytes(row, 0)?;
                let raw_id = match row.get_ref(1).map_err(sql::sql_error)? {
                    ValueRef::Null => None,
                    ValueRef::Text(bytes) => Some(bytes),
                    _ => return Err(corrupt("catalog diagnostic record ID is not text")),
                };
                let raw_code = text_bytes(row, 2)?;
                let raw_details = text_bytes(row, 3)?;
                let bytes = [
                    raw_path.len(),
                    raw_id.map_or(0, <[u8]>::len),
                    raw_code.len(),
                    raw_details.len(),
                ]
                .into_iter()
                .try_fold(0usize, |total, bytes| total.checked_add(bytes))
                .ok_or_else(|| {
                    WikiError::new(
                        ErrorCode::BudgetExceeded,
                        "catalog diagnostic byte count overflow",
                    )
                })?;
                self.reserve(bytes)?;
                if utf8(raw_path)? != path.as_str() {
                    return Err(corrupt(
                        "selected diagnostic path differs from its indexed key",
                    ));
                }
                #[cfg(test)]
                super::query_diagnostics::row(row, row.as_ref().column_count());
                let record_id = raw_id
                    .map(|bytes| {
                        RecordId::new(utf8(bytes)?)
                            .map_err(|_| corrupt("invalid catalog diagnostic record ID"))
                    })
                    .transpose()?;
                let code =
                    serde_json::from_value(serde_json::Value::String(utf8(raw_code)?.to_owned()))
                        .map_err(|error| corrupt(error.to_string()))?;
                let details = serde_json::from_slice(raw_details)
                    .map_err(|error| corrupt(error.to_string()))?;
                result.push(CatalogDiagnostic {
                    path: path.clone(),
                    record_id,
                    code,
                    details,
                });
            }
        }
        Ok(result)
    }
    fn dependency_fingerprint(&self) -> Result<Blake3Hash> {
        // Include physical identity and the actual pinned epoch: rebuilt files
        // can share logical headers/epochs while containing different candidates.
        if let QueryConnection::Normalized(selected) = &self.connection {
            return serde_json::to_vec(&(
                self.query_scope(),
                &self.vault_id,
                &selected.selection().file_id,
                &self.snapshot,
            ))
            .map(Blake3Hash::digest)
            .map_err(|error| WikiError::new(ErrorCode::Internal, error.to_string()));
        }
        // Scope/header commitment only. This is not a global dependency proof.
        serde_json::to_vec(&(self.query_scope(), &self.vault_id, &self.snapshot))
            .map(Blake3Hash::digest)
            .map_err(|error| WikiError::new(ErrorCode::Internal, error.to_string()))
    }
    fn query_scope(&self) -> &'static str {
        match self.scope {
            QueryScope::IndexedEvidence => "indexed_evidence",
            QueryScope::CatalogSnapshot => "catalog_snapshot",
        }
    }
    fn decode_document(&self, row: &Row<'_>, column: usize) -> Result<DocumentRow> {
        if self.normalized_layout() {
            let mut extra = 0usize;
            for index in column + 14..row.as_ref().column_count() {
                if let ValueRef::Text(value) = row.get_ref(index).map_err(sql::sql_error)? {
                    extra = extra.checked_add(value.len()).ok_or_else(|| {
                        WikiError::new(
                            ErrorCode::BudgetExceeded,
                            "normalized selected byte count overflow",
                        )
                    })?;
                }
            }
            return normalized_read::document(row, column, |bytes| {
                self.reserve(bytes.checked_add(extra).ok_or_else(|| {
                    WikiError::new(
                        ErrorCode::BudgetExceeded,
                        "normalized selected byte count overflow",
                    )
                })?)?;
                #[cfg(test)]
                super::query_diagnostics::row(row, row.as_ref().column_count());
                Ok(())
            });
        }
        self.decode(row, column)
    }
}

impl Drop for QuerySnapshot {
    fn drop(&mut self) {
        let _ = self.connection.execute_batch("ROLLBACK");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::{VaultFs, VaultRoot, WriterPermit};
    use std::{collections::BTreeMap, fs, time::Duration};

    fn path(value: &str) -> VaultRelativePath {
        VaultRelativePath::new(value).unwrap()
    }
    fn id(value: &str) -> RecordId {
        RecordId::new(value).unwrap()
    }
    fn page(title: &str) -> String {
        format!(
            "---\nwiki_schema: \"1\"\nwiki_id: page_query\nwiki_kind: page\ntitle: {title}\nwiki_status: reviewed\n---\n# {title}\n\nBounded selected evidence.\n"
        )
    }
    fn unsynced() -> (tempfile::TempDir, VaultRoot, Catalog) {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("WIKI.md"), "---\nwiki_schema: \"1\"\nwiki_id: vault_query\nwiki_kind: vault\ntitle: Bounded query fixture\n---\n").unwrap();
        fs::write(temp.path().join("page.md"), page("Old selected title")).unwrap();
        let root = VaultRoot::explicit(temp.path()).unwrap();
        let catalog = Catalog::new(VaultFs::new(root.clone()), id("vault_query"));
        (temp, root, catalog)
    }
    fn fixture() -> (tempfile::TempDir, VaultRoot, Catalog) {
        let (temp, root, catalog) = unsynced();
        let writer = WriterPermit::acquire(&root, Duration::from_millis(200)).unwrap();
        catalog.sync(&writer).unwrap();
        (temp, root, catalog)
    }
    fn database(catalog: &Catalog) -> Connection {
        Connection::open(catalog.cache_path().unwrap()).unwrap()
    }
    fn defaults(catalog: &Catalog) -> QuerySnapshot {
        catalog.query_snapshot(QueryReadLimits::default()).unwrap()
    }

    fn publish_normalized(
        catalog: &Catalog,
        epoch: u64,
    ) -> super::super::normalized_build::CompletedCatalog {
        publish_normalized_layout(catalog, epoch, false)
    }

    fn publish_normalized_layout(
        catalog: &Catalog,
        epoch: u64,
        with_facts: bool,
    ) -> super::super::normalized_build::CompletedCatalog {
        use crate::catalog::{
            file_types::{BuildIdentity, CatalogSelection},
            normalized_build::{BuildLimits, NormalizedBuilder},
        };
        let writer = WriterPermit::acquire(catalog.fs.root(), Duration::from_secs(1)).unwrap();
        let identity = BuildIdentity {
            selection: CatalogSelection::new(catalog.vault_id.clone(), epoch).unwrap(),
            origin: None,
            vector_cache_lost: false,
            vector_loss_unknown: false,
        };
        selector::prepare(&catalog.fs, &writer, &identity.selection).unwrap();
        let mut builder =
            NormalizedBuilder::begin(&catalog.fs, &writer, identity, BuildLimits::default())
                .unwrap();
        let input = scan::scan_input(&catalog.fs, &catalog.vault_id).unwrap();
        let completed = if with_facts {
            let projection =
                scan::project_normalized_with_sink(&catalog.fs, &input, false, &mut builder)
                    .unwrap();
            builder.finish_normalized(&projection).unwrap()
        } else {
            let projection =
                scan::project_with_sink(&catalog.fs, &input, false, &mut builder).unwrap();
            builder.finish(&projection).unwrap()
        };
        selector::publish(
            &catalog.fs,
            &writer,
            &completed.identity.selection,
            Duration::from_secs(1),
        )
        .unwrap();
        completed
    }

    fn refresh_source_fixture(root: &std::path::Path) -> crate::sources::RevisionSignature {
        fn note(
            root: &std::path::Path,
            name: &str,
            kind: &str,
            id: &str,
            extra: serde_json::Value,
        ) {
            let mut fields = BTreeMap::from([
                ("wiki_schema".into(), serde_json::json!("1")),
                ("wiki_id".into(), serde_json::json!(id)),
                ("wiki_kind".into(), serde_json::json!(kind)),
                ("title".into(), serde_json::json!("Révision 東京")),
            ]);
            fields.extend(
                extra
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone())),
            );
            crate::domain::CanonicalRecord::new(fields.clone()).unwrap();
            let mut text = String::from("---\n");
            for (key, value) in fields {
                text.push_str(&format!("{key}: {value}\n"));
            }
            text.push_str("---\n");
            let target = root.join(name);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(target, text).unwrap();
        }
        note(
            root,
            "sources/source_refresh/source.md",
            "source",
            "source_refresh",
            serde_json::json!({
                "wiki_status":"active", "wiki_origin_kind":"local-file", "wiki_origin":"fixture",
                "wiki_current_revision":"revision_current", "wiki_revisions":["revision_z","revision_a","revision_current","revision_unsupported"]
            }),
        );
        let fingerprint = Blake3Hash::digest(b"fixture-v1");
        for (revision, content, complete) in [
            ("revision_z", "repeat", true),
            ("revision_a", "repeat", true),
            ("revision_current", "head", true),
            ("revision_unsupported", "unavailable", false),
        ] {
            let parent = format!("sources/source_refresh/revisions/{revision}");
            let hash = Blake3Hash::digest(content.as_bytes());
            let mut extra = serde_json::json!({
                "wiki_source_id":"source_refresh", "wiki_captured_at":"2026-09-28T00:00:00Z",
                "wiki_original_path":"original.bin", "wiki_original_hash":hash,
                "wiki_extractor":"fixture", "wiki_extractor_fingerprint":fingerprint,
                "wiki_extraction_status":if complete {"complete"} else {"unsupported"}
            });
            if complete {
                extra["wiki_content_path"] = serde_json::json!("content.md");
                extra["wiki_content_hash"] = serde_json::json!(hash);
            }
            note(
                root,
                &format!("{parent}/revision.md"),
                "revision",
                revision,
                extra,
            );
            fs::write(root.join(format!("{parent}/original.bin")), content).unwrap();
            if complete {
                fs::write(root.join(format!("{parent}/content.md")), content).unwrap();
            }
        }
        crate::sources::RevisionSignature {
            original_hash: Blake3Hash::digest(b"repeat"),
            content_hash: Some(Blake3Hash::digest(b"repeat")),
            extractor_fingerprint: fingerprint,
        }
    }

    #[test]
    fn cached_identity_claim_accepts_unadopted_and_reserves_malformed_duplicates() {
        for duplicate in [false, true] {
            let (temp, _root, catalog) = unsynced();
            fs::write(
                temp.path().join("broken.md"),
                "---\nwiki_id: isolated_id\ntitle: [broken\n---\n",
            )
            .unwrap();
            if duplicate {
                fs::write(
                    temp.path().join("other.md"),
                    "---\nwiki_id: isolated_id\ntitle: [broken\n---\n",
                )
                .unwrap();
            }
            publish_normalized(&catalog, 1);
            let reader = defaults(&catalog);
            if duplicate {
                assert_eq!(
                    reader
                        .unique_identity_claim(&id("isolated_id"))
                        .unwrap_err()
                        .code,
                    ErrorCode::ReferenceAmbiguous
                );
            } else {
                let claim = reader
                    .unique_identity_claim(&id("isolated_id"))
                    .unwrap()
                    .unwrap();
                assert_eq!(claim.path, path("broken.md"));
                assert_eq!(claim.kind, None);
                assert!(reader.record(&id("isolated_id")).unwrap().is_none());
            }
            assert!(
                reader
                    .unique_identity_claim(&id("absent_id"))
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    fn complete_identity_claim_lookup_returns_every_malformed_claimant_path() {
        let (temp, _root, catalog) = unsynced();
        for suffix in ["a", "b", "c"] {
            fs::write(
                temp.path().join(format!("broken_{suffix}.md")),
                "---\nwiki_id: duplicate_claim\ntitle: [broken\n---\n",
            )
            .unwrap();
        }
        let completed = publish_normalized(&catalog, 1);
        let reader = defaults(&catalog);
        let claims = reader
            .identity_claims_for_id(&id("duplicate_claim"))
            .unwrap();
        assert_eq!(
            claims
                .iter()
                .map(|claim| claim.path.clone())
                .collect::<Vec<_>>(),
            [
                path("broken_a.md"),
                path("broken_b.md"),
                path("broken_c.md")
            ]
        );
        assert!(
            claims
                .iter()
                .all(|claim| claim.id == id("duplicate_claim") && claim.kind.is_none())
        );
        assert!(
            reader
                .identity_claims_for_id(&id("absent_claim"))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            reader
                .unique_identity_claim(&id("duplicate_claim"))
                .unwrap_err()
                .code,
            ErrorCode::ReferenceAmbiguous
        );
        let limited = catalog
            .query_snapshot(QueryReadLimits {
                max_rows: 4,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            limited
                .identity_claims_for_id(&id("duplicate_claim"))
                .unwrap_err()
                .code,
            ErrorCode::BudgetExceeded
        );
        assert_eq!(limited.usage().rows, 4);
        // The complete reader must validate a later claim beyond unique's two witnesses.
        let db = Connection::open(completed.path).unwrap();
        db.execute("UPDATE identity_claims SET file_hash='bad' WHERE record_id='duplicate_claim' AND path='broken_c.md'",[]).unwrap();
        let fresh = defaults(&catalog);
        assert_eq!(
            fresh
                .identity_claims_for_id(&id("duplicate_claim"))
                .unwrap_err()
                .code,
            ErrorCode::IndexCorrupt
        );
        assert_eq!(fresh.usage().rows, 5);
    }

    #[test]
    fn cached_identity_claim_meters_corruption_before_decode() {
        for mutation in [
            "UPDATE identity_claims SET file_hash='invalid' WHERE record_id='page_query'",
            "UPDATE identity_claims SET path='../bad.md' WHERE record_id='page_query'",
            "UPDATE identity_claims SET kind='invalid' WHERE record_id='page_query'",
            "UPDATE identity_claims SET kind=x'ff' WHERE record_id='page_query'",
        ] {
            let (_temp, _root, catalog) = unsynced();
            let completed = publish_normalized(&catalog, 1);
            let database = Connection::open(&completed.path).unwrap();
            selector::configure_wal(&database).unwrap();
            database.execute_batch(mutation).unwrap();
            let reader = defaults(&catalog);
            assert_eq!(
                reader
                    .unique_identity_claim(&id("page_query"))
                    .unwrap_err()
                    .code,
                ErrorCode::IndexCorrupt,
                "{mutation}"
            );
            assert_eq!(reader.usage().rows, 3); // table, PK, selected claim
            assert!(reader.usage().bytes > id("page_query").as_str().len());
        }
    }

    #[test]
    fn cached_identity_claim_requires_exact_complete_primary_key() {
        for definition in [
            "",
            "CREATE TABLE identity_claims(record_id TEXT NOT NULL,path TEXT NOT NULL,file_hash TEXT NOT NULL,kind TEXT)",
            "CREATE TABLE identity_claims(record_id TEXT COLLATE NOCASE NOT NULL,path TEXT NOT NULL,file_hash TEXT NOT NULL,kind TEXT,PRIMARY KEY(record_id,path))",
            "CREATE TABLE identity_claims(record_id TEXT NOT NULL,path TEXT NOT NULL,file_hash TEXT NOT NULL,kind TEXT,PRIMARY KEY(path,record_id))",
        ] {
            let (_temp, _root, catalog) = unsynced();
            let completed = publish_normalized(&catalog, 1);
            let database = Connection::open(&completed.path).unwrap();
            selector::configure_wal(&database).unwrap();
            database
                .execute_batch("DROP TABLE identity_claims")
                .unwrap();
            database.execute_batch(definition).unwrap();
            let error = defaults(&catalog)
                .unique_identity_claim(&id("absent_id"))
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::CapabilityUnavailable);
            assert_eq!(error.hint.as_deref(), Some("run index rebuild"));
        }
    }

    #[test]
    fn cached_identity_claim_duplicate_witness_cannot_escape_budget() {
        let (temp, _root, catalog) = unsynced();
        fs::write(temp.path().join("duplicate.md"), page("Duplicate")).unwrap();
        publish_normalized(&catalog, 1);
        let reader = catalog
            .query_snapshot(QueryReadLimits {
                max_rows: 3,
                ..QueryReadLimits::default()
            })
            .unwrap();
        assert_eq!(
            reader
                .unique_identity_claim(&id("page_query"))
                .unwrap_err()
                .code,
            ErrorCode::BudgetExceeded
        );
        assert_eq!(reader.usage().rows, 3);
        let reader = catalog
            .query_snapshot(QueryReadLimits {
                max_bytes: 1,
                ..QueryReadLimits::default()
            })
            .unwrap();
        assert_eq!(
            reader
                .unique_identity_claim(&id("page_query"))
                .unwrap_err()
                .code,
            ErrorCode::BudgetExceeded
        );
        assert_eq!(reader.usage(), QueryReadUsage::default());
    }

    #[test]
    fn cached_identity_claim_work_is_independent_of_unrelated_claims() {
        let (_temp, _root, catalog) = unsynced();
        let completed = publish_normalized(&catalog, 1);
        let before = defaults(&catalog);
        let expected = before.unique_identity_claim(&id("page_query")).unwrap();
        let usage = before.usage();
        drop(before);
        let database = Connection::open(&completed.path).unwrap();
        selector::configure_wal(&database).unwrap();
        database.execute_batch("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<4096) INSERT INTO identity_claims SELECT 'unrelated_'||x,'other_'||x||'.md','malformed','invalid' FROM n").unwrap();
        let after = defaults(&catalog);
        assert_eq!(
            after.unique_identity_claim(&id("page_query")).unwrap(),
            expected
        );
        assert_eq!(after.usage(), usage);
    }

    #[test]
    fn refresh_lookup_real_claims_unicode_and_malformed_duplicates() {
        for variant in 0..3 {
            let (temp, _root, catalog) = unsynced();
            fs::rename(temp.path().join("page.md"), temp.path().join("東京.md")).unwrap();
            fs::write(temp.path().join("東京.md"), page("Résumé 東京")).unwrap();
            if variant == 1 {
                fs::write(temp.path().join("duplicate.md"), page("Duplicate")).unwrap();
            }
            if variant == 2 {
                fs::write(temp.path().join("broken.md"), "---\nwiki_schema: '1'\nwiki_id: page_query\nwiki_kind: invalid\ntitle: Broken\n---\n").unwrap();
            }
            publish_normalized(&catalog, 1);
            let reader = defaults(&catalog);
            let lookup: &dyn crate::sources::SourceRefreshLookup = &reader;
            assert!(lookup.id_is_claimed(&id("page_query")).unwrap());
            if variant == 0 {
                let selected = lookup.unique_record(&id("page_query")).unwrap().unwrap();
                assert_eq!(selected.path, path("東京.md"));
                assert_eq!(selected.record.title(), "Résumé 東京");
                assert_eq!(
                    lookup
                        .record_at_path(&path("東京.md"))
                        .unwrap()
                        .unwrap()
                        .hash,
                    selected.hash
                );
                assert!(lookup.unique_record(&id("not_present")).unwrap().is_none());
                assert!(!lookup.id_is_claimed(&id("not_present")).unwrap());
                assert!(lookup.record_at_path(&path("absent.md")).unwrap().is_none());
                assert_eq!(lookup.snapshot(), QueryCatalog::snapshot(&reader));
                assert_eq!(lookup.vault_id(), &id("vault_query"));
            } else {
                assert_eq!(
                    lookup.unique_record(&id("page_query")).unwrap_err().code,
                    ErrorCode::IndexCorrupt
                );
            }
        }
    }

    #[test]
    fn refresh_lookup_admits_claim_bytes_before_decode_and_refuses_mismatch() {
        for mutation in [
            "UPDATE identity_claims SET file_hash='invalid' WHERE record_id='page_query'",
            "UPDATE identity_claims SET kind=NULL WHERE record_id='page_query'",
            "UPDATE identity_claims SET path='wrong.md' WHERE record_id='page_query'",
            "DELETE FROM identity_claims WHERE record_id='page_query'",
        ] {
            let (_temp, _root, catalog) = unsynced();
            let completed = publish_normalized(&catalog, 1);
            let writer = Connection::open(&completed.path).unwrap();
            selector::configure_wal(&writer).unwrap();
            writer.execute_batch(mutation).unwrap();
            drop(writer);
            let reader = defaults(&catalog);
            let lookup: &dyn crate::sources::SourceRefreshLookup = &reader;
            assert!(lookup.id_is_claimed(&id("page_query")).unwrap());
            assert_eq!(
                lookup.unique_record(&id("page_query")).unwrap_err().code,
                ErrorCode::IndexCorrupt
            );
        }
        let (temp, _root, catalog) = unsynced();
        fs::rename(temp.path().join("page.md"), temp.path().join("東京.md")).unwrap();
        let completed = publish_normalized(&catalog, 1);
        let database = Connection::open(&completed.path).unwrap();
        selector::configure_wal(&database).unwrap();
        database
            .execute("UPDATE records SET row_json='{' WHERE id='page_query'", [])
            .unwrap();
        drop(database);
        let reader = catalog
            .query_snapshot(QueryReadLimits {
                max_rows: 1,
                ..QueryReadLimits::default()
            })
            .unwrap();
        let lookup: &dyn crate::sources::SourceRefreshLookup = &reader;
        assert_eq!(
            lookup.unique_record(&id("page_query")).unwrap_err().code,
            ErrorCode::BudgetExceeded
        );
        let claim_bytes =
            "東京.md".len() + Blake3Hash::digest(b"anything").as_str().len() + "page".len();
        assert_eq!(
            reader.usage(),
            QueryReadUsage {
                rows: 1,
                bytes: claim_bytes
            }
        );
        let limited = catalog
            .query_snapshot(QueryReadLimits {
                max_row_bytes: claim_bytes - 1,
                ..QueryReadLimits::default()
            })
            .unwrap();
        let lookup: &dyn crate::sources::SourceRefreshLookup = &limited;
        assert_eq!(
            lookup.unique_record(&id("page_query")).unwrap_err().code,
            ErrorCode::BudgetExceeded
        );
        assert_eq!(limited.usage().rows, 0);
    }

    #[test]
    fn revision_ownership_lookup_requires_complete_registry_and_bounds_reads() {
        use crate::changes::{RevisionOwnershipLookup, RevisionTreeKey};
        let (_temp, _root, catalog) = unsynced();
        let completed = publish_normalized(&catalog, 1);
        let connection = Connection::open(&completed.path).unwrap();
        let key = RevisionTreeKey {
            source_component: "source_Selected".into(),
            revision_component: "revision_Selected".into(),
        };
        let manifest_hash = Blake3Hash::digest(b"retained owner fixture");
        connection
            .execute(
                "INSERT INTO revision_tree_owners VALUES(?1,?2,?3,?4)",
                params![
                    key.source_component,
                    key.revision_component,
                    "change_owner",
                    manifest_hash.as_str()
                ],
            )
            .unwrap();
        connection.execute(
            "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<1000) INSERT INTO revision_tree_owners SELECT 'unrelated_'||x,'revision_'||x,'change_owner',?1 FROM n",
            [manifest_hash.as_str()],
        ).unwrap();
        let reader = defaults(&catalog);
        let owner = reader.revision_owner(&key).unwrap().unwrap();
        assert_eq!(owner.change_id, id("change_owner"));
        assert_eq!(owner.manifest_hash, manifest_hash);
        assert_eq!(reader.usage().rows, 2); // one readiness row + selected owner
        assert!(
            reader
                .revision_owner(&RevisionTreeKey {
                    source_component: "source_selected".into(),
                    revision_component: key.revision_component.clone(),
                })
                .unwrap()
                .is_none()
        ); // identity components remain case-sensitive
        drop(reader);
        connection
            .execute("UPDATE catalog_meta SET revision_ownership_version=0", [])
            .unwrap();
        let reader = defaults(&catalog);
        assert_eq!(
            reader.revision_owner(&key).unwrap_err().code,
            ErrorCode::OfflineUnavailable
        );
        assert_eq!(
            reader.require_ready().unwrap_err().code,
            ErrorCode::OfflineUnavailable
        );
        drop(reader);
        connection
            .execute("UPDATE catalog_meta SET revision_ownership_version=1", [])
            .unwrap();
        let limited = catalog
            .query_snapshot(QueryReadLimits {
                max_rows: 1,
                ..QueryReadLimits::default()
            })
            .unwrap();
        assert_eq!(
            limited.revision_owner(&key).unwrap_err().code,
            ErrorCode::BudgetExceeded
        );
    }

    #[test]
    fn refresh_revision_signature_uses_retained_order_and_validates_tuple() {
        let (temp, _root, catalog) = unsynced();
        let signature = refresh_source_fixture(temp.path());
        let completed = publish_normalized(&catalog, 1);
        let reader = defaults(&catalog);
        let lookup: &dyn crate::sources::SourceRefreshLookup = &reader;
        let source = id("source_refresh");
        let matched = lookup
            .matching_revision(&source, &signature)
            .unwrap()
            .unwrap();
        assert_eq!(matched.retained_ordinal, 0);
        assert_eq!(matched.revision.record.id(), &id("revision_z")); // retained order differs from ID order
        assert_eq!(
            lookup
                .unique_record(&source)
                .unwrap()
                .unwrap()
                .record
                .string("wiki_current_revision"),
            Some("revision_current")
        );
        for (bytes, content, expected) in [
            (b"head".as_slice(), true, "revision_current"),
            (b"unavailable".as_slice(), false, "revision_unsupported"),
        ] {
            let requested = crate::sources::RevisionSignature {
                original_hash: Blake3Hash::digest(bytes),
                content_hash: content.then(|| Blake3Hash::digest(bytes)),
                extractor_fingerprint: signature.extractor_fingerprint.clone(),
            };
            assert_eq!(
                lookup
                    .matching_revision(&source, &requested)
                    .unwrap()
                    .unwrap()
                    .revision
                    .record
                    .id(),
                &id(expected)
            );
        }
        let wrong = crate::sources::RevisionSignature {
            original_hash: signature.original_hash.clone(),
            content_hash: None,
            extractor_fingerprint: signature.extractor_fingerprint.clone(),
        };
        assert!(lookup.matching_revision(&source, &wrong).unwrap().is_none());
        drop(reader);
        let writer = Connection::open(&completed.path).unwrap();
        selector::configure_wal(&writer).unwrap();
        writer.execute("UPDATE source_revision_identity SET extraction_status='unsupported' WHERE revision_id='revision_z'", []).unwrap();
        drop(writer);
        let reader = defaults(&catalog);
        let lookup: &dyn crate::sources::SourceRefreshLookup = &reader;
        assert_eq!(
            lookup
                .matching_revision(&source, &signature)
                .unwrap_err()
                .code,
            ErrorCode::IndexCorrupt
        );
    }

    #[test]
    fn refresh_assertions_stream_distinct_ids_with_admission_and_indexed_plan() {
        let (_temp, _root, catalog) = unsynced();
        let completed = publish_normalized(&catalog, 1);
        let database = Connection::open(&completed.path).unwrap();
        selector::configure_wal(&database).unwrap();
        database.execute_batch("INSERT INTO source_evidence VALUES('source_selected','evidence_b','assertion_b'),('source_selected','evidence_a','assertion_a'),('source_selected','evidence_c','assertion_b'); WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1000) INSERT INTO source_evidence SELECT 'unrelated_'||x,'evidence_'||x,'assertion_'||x FROM n;").unwrap();
        drop(database);
        let reader = defaults(&catalog);
        let lookup: &dyn crate::sources::SourceRefreshLookup = &reader;
        assert_eq!(
            lookup.source_assertions(&id("source_selected")).unwrap(),
            vec![id("assertion_a"), id("assertion_b")]
        );
        assert_eq!(
            reader.usage(),
            QueryReadUsage {
                rows: 2,
                bytes: "assertion_a".len() + "assertion_b".len()
            }
        );
        for (query, index) in [
            (
                "SELECT DISTINCT assertion_id FROM source_evidence INDEXED BY source_assertions WHERE source_id='source_selected' ORDER BY assertion_id",
                "source_assertions",
            ),
            (
                "SELECT revision_id FROM source_revision_identity INDEXED BY source_revision_matches WHERE source_id='source_selected' AND original_hash='hash' AND content_hash IS NULL AND extractor_fingerprint='fingerprint' ORDER BY retained_ordinal LIMIT 1",
                "source_revision_matches",
            ),
            (
                "SELECT id FROM records INDEXED BY record_paths WHERE path='page.md'",
                "record_paths",
            ),
        ] {
            let mut statement = reader
                .connection()
                .prepare(&format!("EXPLAIN QUERY PLAN {query}"))
                .unwrap();
            let details = statement
                .query_map([], |row| row.get::<_, String>(3))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap();
            assert!(
                details
                    .iter()
                    .any(|detail| detail.contains("SEARCH") && detail.contains(index)),
                "{details:?}"
            );
            assert!(
                !details.iter().any(|detail| detail.contains("TEMP B-TREE")),
                "{details:?}"
            );
        }
        let limited = catalog
            .query_snapshot(QueryReadLimits {
                max_rows: 1,
                ..QueryReadLimits::default()
            })
            .unwrap();
        let lookup: &dyn crate::sources::SourceRefreshLookup = &limited;
        assert_eq!(
            lookup
                .source_assertions(&id("source_selected"))
                .unwrap_err()
                .code,
            ErrorCode::BudgetExceeded
        );
        assert_eq!(limited.usage().rows, 1);
    }

    #[test]
    fn refresh_rejects_legacy_unbound_and_known_invalid_sources() {
        let (_temp, _root, catalog) = fixture();
        let reader = defaults(&catalog);
        let lookup: &dyn crate::sources::SourceRefreshLookup = &reader;
        assert_eq!(
            lookup.unique_record(&id("page_query")).unwrap_err().code,
            ErrorCode::OfflineUnavailable
        );
        let (temp, _root, catalog) = unsynced();
        refresh_source_fixture(temp.path());
        fs::write(
            temp.path()
                .join("sources/source_refresh/revisions/revision_z/original.bin"),
            "corrupt retained payload",
        )
        .unwrap();
        publish_normalized(&catalog, 1);
        let reader = defaults(&catalog);
        let lookup: &dyn crate::sources::SourceRefreshLookup = &reader;
        assert_eq!(
            lookup
                .unique_record(&id("source_refresh"))
                .unwrap_err()
                .code,
            ErrorCode::SourceIntegrity
        );
        drop(reader);
        let mut unbound = defaults(&catalog);
        unbound.snapshot = ReadSnapshot::canonical(
            1,
            scan::parser_fingerprint(),
            Blake3Hash::digest(b"not publication"),
        );
        let lookup: &dyn crate::sources::SourceRefreshLookup = &unbound;
        assert_eq!(
            lookup
                .id_is_claimed(&id("source_refresh"))
                .unwrap_err()
                .code,
            ErrorCode::IndexCorrupt
        );
    }

    #[test]
    fn selected_v2_uses_bounded_normalized_rows_records_and_diagnostics() {
        let (_temp, _root, catalog) = unsynced();
        let completed = publish_normalized(&catalog, 1);
        let database = Connection::open(&completed.path).unwrap();
        database.execute("INSERT INTO diagnostics(path,record_id,code,details_json) VALUES('page.md','page_query','INDEX_CORRUPT','{}')", []).unwrap();
        // An unrelated invalid payload must not affect selected lookups.
        database
            .execute("UPDATE records SET row_json='{' WHERE id='vault_query'", [])
            .unwrap();
        let reader = defaults(&catalog);
        assert!(reader.normalized_layout());
        assert_eq!(
            reader.publication_id(),
            Some(completed.identity.selection.file_id.as_str())
        );
        assert_eq!(reader.verification(), &SnapshotVerification::IndexSnapshot);
        assert_eq!(reader.usage(), QueryReadUsage::default());
        assert_eq!(
            reader.document(&path("page.md")).unwrap().unwrap().title,
            "Old selected title"
        );
        assert_eq!(
            reader
                .record(&id("page_query"))
                .unwrap()
                .unwrap()
                .record
                .title(),
            "Old selected title"
        );
        assert_eq!(
            reader
                .diagnostics(&BTreeSet::from([path("page.md")]))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(reader.usage().rows, 3);
        assert!(reader.usage().bytes > 0);
        assert_eq!(
            reader.record(&id("vault_query")).unwrap_err().code,
            ErrorCode::IndexCorrupt
        );
        assert_eq!(reader.usage().rows, 4);
        assert!(
            reader
                .connection()
                .execute("DELETE FROM documents", [])
                .is_err()
        );
    }

    #[test]
    fn selected_v2_reserves_before_json_and_enforces_aggregate_limits() {
        let (_temp, _root, catalog) = unsynced();
        let completed = publish_normalized(&catalog, 1);
        let database = Connection::open(&completed.path).unwrap();
        database
            .execute(
                "UPDATE documents SET aliases_json='{' WHERE path='page.md'",
                [],
            )
            .unwrap();
        let reader = catalog
            .query_snapshot(QueryReadLimits {
                max_row_bytes: 1,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            reader.document(&path("page.md")).unwrap_err().code,
            ErrorCode::BudgetExceeded
        );
        assert_eq!(reader.usage(), QueryReadUsage::default());
        drop(reader);
        let reader = defaults(&catalog);
        assert_eq!(
            reader.document(&path("page.md")).unwrap_err().code,
            ErrorCode::IndexCorrupt
        );
        assert_eq!(reader.usage().rows, 1);
        drop(reader);
        database
            .execute(
                "UPDATE documents SET aliases_json='[]' WHERE path='page.md'",
                [],
            )
            .unwrap();
        let reader = catalog
            .query_snapshot(QueryReadLimits {
                max_rows: 1,
                ..Default::default()
            })
            .unwrap();
        reader.document(&path("page.md")).unwrap().unwrap();
        assert_eq!(
            reader.record(&id("page_query")).unwrap_err().code,
            ErrorCode::BudgetExceeded
        );
        let bytes = reader.usage().bytes;
        let reader = catalog
            .query_snapshot(QueryReadLimits {
                max_bytes: bytes,
                ..Default::default()
            })
            .unwrap();
        reader.document(&path("page.md")).unwrap().unwrap();
        assert_eq!(
            reader.document(&path("page.md")).unwrap_err().code,
            ErrorCode::BudgetExceeded
        );
    }

    #[test]
    fn selected_v2_validates_records_and_bounded_diagnostic_index() {
        for mutation in [
            "UPDATE records SET path='tampered.md' WHERE id='page_query'",
            "UPDATE records SET disputed=1 WHERE id='page_query'",
            "UPDATE records SET identity_eligibility='current' WHERE id='page_query'",
            "UPDATE records SET row_json='{' WHERE id='page_query'",
        ] {
            let (_temp, _root, catalog) = unsynced();
            let completed = publish_normalized(&catalog, 1);
            let writer = Connection::open(completed.path).unwrap();
            selector::configure_wal(&writer).unwrap();
            writer.execute_batch(mutation).unwrap();
            drop(writer);
            let reader = defaults(&catalog);
            assert_eq!(
                reader.record(&id("page_query")).unwrap_err().code,
                ErrorCode::IndexCorrupt,
                "{mutation}"
            );
            assert_eq!(reader.usage().rows, 1);
        }
        let (_temp, _root, catalog) = unsynced();
        let completed = publish_normalized(&catalog, 1);
        let writer = Connection::open(completed.path).unwrap();
        selector::configure_wal(&writer).unwrap();
        writer
            .execute_batch(
                "DROP INDEX diagnostic_paths; CREATE INDEX diagnostic_paths ON diagnostics(code)",
            )
            .unwrap();
        drop(writer);
        assert_eq!(
            defaults(&catalog)
                .diagnostics(&BTreeSet::from([path("page.md")]))
                .unwrap_err()
                .code,
            ErrorCode::CapabilityUnavailable
        );
    }

    #[test]
    fn selected_v2_rejects_partial_collated_and_oversized_diagnostic_index_definitions() {
        for replacement in [
            String::new(),
            "CREATE INDEX diagnostic_paths ON diagnostics(path) WHERE code='NONE'".into(),
            "CREATE INDEX diagnostic_paths ON diagnostics(path COLLATE NOCASE)".into(),
            "CREATE UNIQUE INDEX diagnostic_paths ON diagnostics(path)".into(),
            format!(
                "CREATE INDEX diagnostic_paths ON diagnostics(path) /*{}*/",
                "x".repeat(4096)
            ),
        ] {
            let (_temp, _root, catalog) = unsynced();
            let completed = publish_normalized(&catalog, 1);
            let database = Connection::open(completed.path).unwrap();
            database.execute("INSERT INTO diagnostics(path,record_id,code,details_json) VALUES('page.md',NULL,'INDEX_CORRUPT','{}')", []).unwrap();
            database
                .execute_batch("DROP INDEX diagnostic_paths")
                .unwrap();
            database.execute_batch(&replacement).unwrap();
            let reader = defaults(&catalog);
            let error = reader
                .diagnostics(&BTreeSet::from([path("page.md")]))
                .unwrap_err();
            assert_eq!(
                error.code,
                ErrorCode::CapabilityUnavailable,
                "{replacement}"
            );
            assert_eq!(error.hint.as_deref(), Some("run index rebuild"));
            assert_eq!(reader.usage(), QueryReadUsage::default());
        }
    }

    #[test]
    fn registry_replacement_excludes_owners_before_saturating_bucket() {
        use crate::catalog::{
            link_facts::{MatchKey, MatchKeyKind},
            navigation_resolution::RegistryProbe,
        };
        let (temp, _root, catalog) = unsynced();
        for suffix in ["a", "b", "c"] {
            fs::write(temp.path().join(format!("candidate_{suffix}.md")), format!("---\nwiki_schema: \"1\"\nwiki_id: candidate_{suffix}\nwiki_kind: page\ntitle: Candidate {suffix}\naliases: [Shared]\nwiki_status: reviewed\n---\n# Candidate\n")).unwrap();
        }
        publish_normalized_layout(&catalog, 1, true);
        let key = MatchKey {
            kind: MatchKeyKind::Alias,
            value: "Shared".into(),
        };
        let reader = defaults(&catalog);
        assert!(matches!(
            reader.registry_probe_for_key(&key).unwrap(),
            RegistryProbe::Many(_)
        ));
        assert!(matches!(
            reader
                .registry_probe_for_key_excluding(&key, &BTreeSet::from([path("candidate_a.md")]))
                .unwrap(),
            RegistryProbe::Many(_)
        ));
        let excluded = BTreeSet::from([path("candidate_a.md"), path("candidate_b.md")]);
        let surviving = reader
            .registry_probe_for_key_excluding(&key, &excluded)
            .unwrap();
        assert!(
            matches!(surviving, RegistryProbe::One(ref candidate) if candidate.id == id("candidate_c") && candidate.path == path("candidate_c.md") && candidate.kind == RecordKind::Page)
        );
        let mut all = excluded;
        all.insert(path("candidate_c.md"));
        assert_eq!(
            reader.registry_probe_for_key_excluding(&key, &all).unwrap(),
            RegistryProbe::Zero
        );
        // An ID probe carries the same adopted kind/path without changing caller precedence.
        let named = MatchKey {
            kind: MatchKeyKind::Id,
            value: "candidate_c".into(),
        };
        assert!(
            matches!(reader.registry_probe_for_key_excluding(&named, &BTreeSet::from([path("candidate_a.md")])).unwrap(), RegistryProbe::One(candidate) if candidate.id == id("candidate_c") && candidate.kind == RecordKind::Page)
        );
        let sixteen: BTreeSet<_> = (0..16)
            .map(|n| path(&format!("unrelated_{n}.md")))
            .collect();
        assert!(matches!(
            reader
                .registry_probe_for_key_excluding(&key, &sixteen)
                .unwrap(),
            RegistryProbe::Many(_)
        ));
        let mut seventeen = sixteen;
        seventeen.insert(path("unrelated_extra.md"));
        let before = reader.usage();
        assert_eq!(
            reader
                .registry_probe_for_key_excluding(&key, &seventeen)
                .unwrap_err()
                .code,
            ErrorCode::BudgetExceeded
        );
        assert_eq!(reader.usage(), before);
    }

    #[test]
    fn registry_exclusion_sql_keeps_indexed_order_and_bound_parameters() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE registry_match_keys(kind TEXT NOT NULL,value TEXT NOT NULL,record_id TEXT NOT NULL,path TEXT NOT NULL,PRIMARY KEY(kind,value,record_id,path)); CREATE TABLE records(id TEXT PRIMARY KEY,kind TEXT NOT NULL,path TEXT NOT NULL);").unwrap();
        for exclusions in [0, 2, 16] {
            let query = registry_probe_sql(exclusions);
            assert_eq!(
                db.prepare(&query).unwrap().parameter_count(),
                2 + exclusions
            );
            let params: Vec<&str> = (0..2 + exclusions).map(|_| "scope").collect();
            let details: Vec<String> = db
                .prepare(&format!("EXPLAIN QUERY PLAN {query}"))
                .unwrap()
                .query_map(rusqlite::params_from_iter(params), |r| r.get(3))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            assert!(
                details.iter().any(|d| d.contains(
                    "SEARCH k USING COVERING INDEX sqlite_autoindex_registry_match_keys_1"
                )),
                "{details:?}"
            );
            assert!(
                details
                    .iter()
                    .any(|d| d.contains("SEARCH r USING INDEX sqlite_autoindex_records_1")),
                "{details:?}"
            );
            assert!(
                !details
                    .iter()
                    .any(|d| d.contains("SCAN") || d.contains("TEMP B-TREE")),
                "{details:?}"
            );
        }
    }

    fn operation_change() -> crate::changes::PreparedChange {
        crate::changes::PreparedChange {
            change_id: id("change_query_authority"),
            manifest_hash: Blake3Hash::digest("query operation"),
        }
    }

    #[test]
    fn selected_v2_does_not_enumerate_unrelated_history_after_activation() {
        let (temp, _root, catalog) = unsynced();
        publish_normalized(&catalog, 1);
        let history = temp.path().join("changes");
        for n in 0..512 {
            let directory = history.join(format!("invalid-history-{n}"));
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join("manifest.json"), b"invalid JSON").unwrap();
            fs::write(directory.join("journal.bin"), b"invalid journal").unwrap();
        }
        #[cfg(unix)]
        let unreadable = {
            use std::os::unix::fs::PermissionsExt;
            let path = history.join("invalid-history-0");
            let permissions = fs::metadata(&path).unwrap().permissions();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
            (path, permissions)
        };
        let reader = defaults(&catalog);
        reader.verify_operations(&catalog).unwrap();
        assert_eq!(
            reader.document(&path("page.md")).unwrap().unwrap().title,
            "Old selected title"
        );
        #[cfg(unix)]
        fs::set_permissions(unreadable.0, unreadable.1).unwrap();
    }

    #[test]
    fn selected_v2_missing_authority_and_authority_without_selector_refuse() {
        let (temp, _root, catalog) = unsynced();
        publish_normalized(&catalog, 1);
        let held = defaults(&catalog);
        fs::remove_file(temp.path().join(".wiki/state/operations.json")).unwrap();
        assert_eq!(
            catalog
                .query_snapshot(QueryReadLimits::default())
                .err()
                .unwrap()
                .code,
            ErrorCode::RecoveryRequired
        );
        assert_eq!(
            held.verify_operations(&catalog).unwrap_err().code,
            ErrorCode::RecoveryRequired
        );

        let (temp, _root, catalog) = unsynced();
        publish_normalized(&catalog, 1);
        for name in [
            "catalog-current.json",
            "catalog-v2-active.json",
            "catalog-acquisition.lock",
        ] {
            fs::remove_file(temp.path().join(".wiki/cache").join(name)).unwrap();
        }
        assert_eq!(
            catalog
                .query_snapshot(QueryReadLimits::default())
                .err()
                .unwrap()
                .code,
            ErrorCode::RecoveryRequired
        );
    }

    #[test]
    fn selected_v2_active_and_cancelled_operations_preserve_coherent_held_reader() {
        use crate::changes::operation_authority;
        let (_temp, root, catalog) = unsynced();
        publish_normalized(&catalog, 1);
        let held = defaults(&catalog);
        let idle = catalog.operation_state().unwrap().unwrap();
        let writer = WriterPermit::acquire(&root, Duration::from_secs(1)).unwrap();
        let intended = Publication {
            file_id: idle.publication().file_id.clone(),
            epoch: 2,
        };
        let active =
            operation_authority::begin(&catalog.fs, &writer, &idle, operation_change(), intended)
                .unwrap();
        let during = defaults(&catalog);
        assert_eq!(during.snapshot(), held.snapshot());
        assert_eq!(held.pending_operation_at_start(), None);
        assert_eq!(
            during.pending_operation_at_start(),
            Some(operation_change().change_id)
        );
        held.verify_operations(&catalog).unwrap();
        during.verify_operations(&catalog).unwrap();
        operation_authority::cancel(&catalog.fs, &writer, &active, &operation_change()).unwrap();
        held.verify_operations(&catalog).unwrap();
        during.verify_operations(&catalog).unwrap();
        assert_eq!(
            during.pending_operation_at_start(),
            Some(operation_change().change_id)
        );
        defaults(&catalog).verify_operations(&catalog).unwrap();
    }

    #[test]
    fn selected_v2_acknowledged_floor_applies_at_start_and_preserves_preack_reader() {
        use crate::changes::operation_authority;
        let (_temp, root, catalog) = unsynced();
        publish_normalized(&catalog, 1);
        let held = defaults(&catalog);
        let idle = catalog.operation_state().unwrap().unwrap();
        let writer = WriterPermit::acquire(&root, Duration::from_secs(1)).unwrap();
        let intended = Publication {
            file_id: idle.publication().file_id.clone(),
            epoch: 2,
        };
        let active = operation_authority::begin(
            &catalog.fs,
            &writer,
            &idle,
            operation_change(),
            intended.clone(),
        )
        .unwrap();
        operation_authority::acknowledge(
            &catalog.fs,
            &writer,
            &active,
            &operation_change(),
            intended,
        )
        .unwrap();
        // A unit adversary advances only the authority; the still-selected SQL
        // epoch must be rejected even before canonical evidence is requested.
        assert_eq!(
            catalog
                .query_snapshot(QueryReadLimits::default())
                .err()
                .unwrap()
                .code,
            ErrorCode::RecoveryRequired
        );
        held.verify_operations(&catalog).unwrap();
    }

    #[test]
    fn legacy_held_reader_rejects_normalized_activation_race() {
        let (_temp, _root, catalog) = fixture();
        let held = defaults(&catalog);
        assert!(!held.normalized_layout());
        held.verify_operations(&catalog).unwrap();
        publish_normalized(&catalog, 1);
        assert_eq!(
            held.verify_operations(&catalog).unwrap_err().code,
            ErrorCode::RecoveryRequired
        );
    }

    #[test]
    fn selected_v2_old_reader_holds_lease_and_rebuild_advances_epoch() {
        let (temp, root, catalog) = unsynced();
        let first = publish_normalized(&catalog, 1);
        let held = defaults(&catalog);
        let fingerprint = held.dependency_fingerprint().unwrap();
        fs::write(temp.path().join("page.md"), page("New selected title")).unwrap();
        let second = publish_normalized(&catalog, 2);
        let current = defaults(&catalog);
        assert_eq!(
            held.snapshot().generation + 1,
            current.snapshot().generation
        );
        held.verify_operations(&catalog).unwrap();
        current.verify_operations(&catalog).unwrap();
        assert_ne!(held.publication_id(), current.publication_id());
        assert_ne!(fingerprint, current.dependency_fingerprint().unwrap());
        assert_eq!(
            held.document(&path("page.md")).unwrap().unwrap().title,
            "Old selected title"
        );
        assert_eq!(
            current.document(&path("page.md")).unwrap().unwrap().title,
            "New selected title"
        );
        let writer = WriterPermit::acquire(&root, Duration::from_secs(1)).unwrap();
        assert!(
            !selector::retire(
                &catalog.fs,
                &writer,
                &catalog.vault_id,
                &first.identity.selection,
                Duration::ZERO
            )
            .unwrap()
        );
        drop(held);
        assert!(
            selector::retire(
                &catalog.fs,
                &writer,
                &catalog.vault_id,
                &first.identity.selection,
                Duration::ZERO
            )
            .unwrap()
        );
        assert!(second.path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn selected_wal_read_only_directory_preserves_data_and_sidecar_identity() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let (temp, _root, catalog) = unsynced();
        let completed = publish_normalized(&catalog, 1);
        let directory = completed.path.parent().unwrap();
        let snapshot = || {
            fs::read_dir(directory)
                .unwrap()
                .map(|entry| {
                    let path = entry.unwrap().path();
                    let metadata = fs::metadata(&path).unwrap();
                    // SQLite readers coordinate through SHM read marks. Its bytes
                    // and mtime may change, while DB/WAL and every file identity
                    // remain unchanged and no directory entry is created.
                    let content = if path.to_string_lossy().ends_with(".sqlite-shm") {
                        None
                    } else {
                        Some((
                            Blake3Hash::digest(fs::read(&path).unwrap()),
                            metadata.modified().unwrap(),
                        ))
                    };
                    (
                        path,
                        (metadata.ino(), metadata.len(), metadata.mode(), content),
                    )
                })
                .collect::<BTreeMap<_, _>>()
        };
        let before = snapshot();
        let permissions = fs::metadata(directory).unwrap().permissions();
        fs::set_permissions(directory, fs::Permissions::from_mode(0o500)).unwrap();
        let reader = defaults(&catalog);
        reader.document(&path("page.md")).unwrap().unwrap();
        reader.record(&id("page_query")).unwrap().unwrap();
        drop(reader);
        fs::set_permissions(directory, permissions).unwrap();
        assert_eq!(before, snapshot());
        assert!(!temp.path().join(".wiki/cache/index.sqlite").exists());
    }

    #[test]
    fn cached_discovery_scope_cannot_share_evidence_fingerprints() {
        for normalized in [false, true] {
            let (_temp, _root, catalog) = fixture();
            if normalized {
                publish_normalized(&catalog, 1);
            }
            let evidence = catalog.query_snapshot(QueryReadLimits::default()).unwrap();
            let cached = catalog
                .cached_query_snapshot(QueryReadLimits::default())
                .unwrap();
            assert_eq!(evidence.query_scope(), "indexed_evidence");
            assert_eq!(cached.query_scope(), "catalog_snapshot");
            assert_eq!(evidence.snapshot(), cached.snapshot());
            assert_eq!(evidence.publication_id(), cached.publication_id());
            assert_ne!(
                evidence.dependency_fingerprint().unwrap(),
                cached.dependency_fingerprint().unwrap()
            );
            assert_eq!(cached.usage(), QueryReadUsage::default());
            assert_eq!(
                evidence.document(&path("page.md")).unwrap(),
                cached.document(&path("page.md")).unwrap()
            );
            cached.verify_operations(&catalog).unwrap();
        }
    }

    #[test]
    fn normalized_selected_queries_count_rows_and_bytes() {
        let (_temp, _root, catalog) = fixture();
        let expected = catalog.index_snapshot().unwrap();
        let reader = defaults(&catalog);
        assert_eq!(reader.usage(), QueryReadUsage::default());
        assert_eq!(reader.snapshot(), expected.snapshot());
        assert_eq!(reader.verification(), &SnapshotVerification::IndexSnapshot);
        assert_eq!(reader.query_scope(), "indexed_evidence");
        let document = reader.document(&path("page.md")).unwrap().unwrap();
        assert_eq!(
            &document,
            expected
                .projection()
                .documents
                .iter()
                .find(|row| row.path == path("page.md"))
                .unwrap()
        );
        let record = reader.record(&id("page_query")).unwrap().unwrap();
        assert_eq!(&record, &expected.projection().records[&id("page_query")]);
        let bytes: usize = database(&catalog).query_row(
            "SELECT (SELECT length(CAST(row_json AS BLOB)) FROM documents WHERE path='page.md') \
             +(SELECT length(CAST(row_json AS BLOB)) FROM records WHERE id='page_query')",
            [], |row| row.get::<_, i64>(0),
        ).unwrap().try_into().unwrap();
        assert_eq!(reader.usage(), QueryReadUsage { rows: 2, bytes });
        assert!(reader.document(&path("absent.md")).unwrap().is_none());
        assert!(reader.record(&id("absent")).unwrap().is_none());
        assert_eq!(reader.usage().rows, 2);
        assert!(
            reader
                .connection()
                .execute("DELETE FROM documents", [])
                .is_err()
        );
    }

    #[test]
    fn invalid_limits_and_missing_cache_fail_without_creating_state() {
        let (temp, _root, catalog) = unsynced();
        let limits = QueryReadLimits {
            max_rows: 0,
            ..QueryReadLimits::default()
        };
        assert_eq!(
            catalog.query_snapshot(limits).err().unwrap().code,
            ErrorCode::Usage
        );
        assert_eq!(
            catalog
                .query_snapshot(QueryReadLimits::default())
                .err()
                .unwrap()
                .code,
            ErrorCode::OfflineUnavailable
        );
        assert!(!temp.path().join(".wiki").exists());
        for limits in [
            QueryReadLimits {
                max_rows: 4097,
                ..QueryReadLimits::default()
            },
            QueryReadLimits {
                max_bytes: 0,
                ..QueryReadLimits::default()
            },
            QueryReadLimits {
                max_row_bytes: 0,
                ..QueryReadLimits::default()
            },
        ] {
            assert_eq!(
                catalog.query_snapshot(limits).err().unwrap().code,
                ErrorCode::Usage
            );
        }
    }

    #[test]
    fn oversized_selected_rows_fail_before_json_decoding() {
        for table in ["documents", "records"] {
            let (_temp, _root, catalog) = fixture();
            database(&catalog)
                .execute(
                    &format!("UPDATE {table} SET row_json=?1"),
                    ["x".repeat(4096)],
                )
                .unwrap();
            let reader = catalog
                .query_snapshot(QueryReadLimits {
                    max_row_bytes: 1024,
                    ..QueryReadLimits::default()
                })
                .unwrap();
            let error = if table == "documents" {
                reader.document(&path("page.md")).err().unwrap()
            } else {
                reader.record(&id("page_query")).err().unwrap()
            };
            assert_eq!(error.code, ErrorCode::BudgetExceeded);
            assert_eq!(reader.usage(), QueryReadUsage::default());
        }
    }

    #[test]
    fn malformed_selected_rows_consume_budget_and_fail_closed() {
        for table in ["documents", "records"] {
            let (_temp, _root, catalog) = fixture();
            database(&catalog)
                .execute(&format!("UPDATE {table} SET row_json='{{'"), [])
                .unwrap();
            let reader = defaults(&catalog);
            let error = if table == "documents" {
                reader.document(&path("page.md")).err().unwrap()
            } else {
                reader.record(&id("page_query")).err().unwrap()
            };
            assert_eq!(error.code, ErrorCode::IndexCorrupt);
            assert_eq!(reader.usage(), QueryReadUsage { rows: 1, bytes: 1 });
        }
    }

    #[test]
    fn non_text_and_invalid_utf8_selected_rows_fail_closed() {
        for mutation in [
            "UPDATE documents SET row_json=x'ff' WHERE path='page.md'",
            "UPDATE documents SET row_json=CAST(x'ff' AS TEXT) WHERE path='page.md'",
        ] {
            let (_temp, _root, catalog) = fixture();
            database(&catalog).execute_batch(mutation).unwrap();
            assert_eq!(
                defaults(&catalog)
                    .document(&path("page.md"))
                    .err()
                    .unwrap()
                    .code,
                ErrorCode::IndexCorrupt
            );
        }
    }

    #[test]
    fn aggregate_row_and_byte_budgets_cover_repeated_reads() {
        let (_temp, _root, catalog) = fixture();
        let bytes: usize = database(&catalog)
            .query_row(
                "SELECT length(CAST(row_json AS BLOB)) FROM documents WHERE path='page.md'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
            .try_into()
            .unwrap();
        for limits in [
            QueryReadLimits {
                max_rows: 1,
                ..QueryReadLimits::default()
            },
            QueryReadLimits {
                max_bytes: 2 * bytes - 1,
                ..QueryReadLimits::default()
            },
        ] {
            let reader = catalog.query_snapshot(limits).unwrap();
            reader.document(&path("page.md")).unwrap();
            assert_eq!(
                reader.document(&path("page.md")).err().unwrap().code,
                ErrorCode::BudgetExceeded
            );
            assert_eq!(reader.usage(), QueryReadUsage { rows: 1, bytes });
        }
    }

    #[test]
    fn header_rejects_old_or_damaged_schema_pointer_and_hashes() {
        for (mutation, code) in [
            ("PRAGMA user_version=999", ErrorCode::CapabilityUnavailable),
            ("DROP TABLE documents", ErrorCode::IndexCorrupt),
            (
                "UPDATE index_meta SET published_gen=NULL",
                ErrorCode::OfflineUnavailable,
            ),
            (
                "UPDATE index_meta SET published_gen=999999",
                ErrorCode::OfflineUnavailable,
            ),
            (
                "UPDATE generations SET state='building'",
                ErrorCode::OfflineUnavailable,
            ),
            (
                "UPDATE generations SET parser_hash='invalid'",
                ErrorCode::IndexCorrupt,
            ),
            (
                "UPDATE generations SET manifest_hash='invalid'",
                ErrorCode::IndexCorrupt,
            ),
            (
                "UPDATE generations SET parser_hash='blake3:0000000000000000000000000000000000000000000000000000000000000000'",
                ErrorCode::OfflineUnavailable,
            ),
        ] {
            let (_temp, _root, catalog) = fixture();
            let db = database(&catalog);
            // Deliberately inject corrupt cache states that normal writes forbid.
            db.pragma_update(None, "foreign_keys", false).unwrap();
            db.execute_batch(mutation).unwrap();
            assert_eq!(
                catalog
                    .query_snapshot(QueryReadLimits::default())
                    .err()
                    .unwrap()
                    .code,
                code,
                "{mutation}"
            );
        }
    }

    #[test]
    fn unselected_corruption_is_outside_selected_integrity_scope() {
        let (_temp, _root, catalog) = fixture();
        let db = database(&catalog);
        db.pragma_update(None, "foreign_keys", false).unwrap();
        db.execute_batch("UPDATE generations SET projection_json='{'; UPDATE documents SET row_json='{' WHERE path='WIKI.md'; UPDATE records SET row_json='{' WHERE id='vault_query'; INSERT INTO dependencies VALUES(999999,'','orphan.md',NULL,'manifest');").unwrap();
        let reader = defaults(&catalog);
        assert_eq!(
            reader.document(&path("page.md")).unwrap().unwrap().title,
            "Old selected title"
        );
        assert!(reader.record(&id("page_query")).unwrap().is_some());
        assert_eq!(reader.usage().rows, 2);
        assert_eq!(
            catalog.index_snapshot().err().unwrap().code,
            ErrorCode::IndexCorrupt
        );
        // No projection JSON, global FK audit, or unselected-row reconstruction.
        assert_eq!(reader.verification(), &SnapshotVerification::IndexSnapshot);
    }

    #[test]
    fn selected_lookup_rejects_mismatched_json_keys() {
        for mutation in [
            "UPDATE documents SET row_json=replace(row_json,'page.md','other.md') WHERE path='page.md'",
            "UPDATE records SET row_json=replace(row_json,'page_query','other_query') WHERE id='page_query'",
        ] {
            let (_temp, _root, catalog) = fixture();
            database(&catalog).execute_batch(mutation).unwrap();
            let reader = defaults(&catalog);
            let error = if mutation.contains("documents") {
                reader.document(&path("page.md")).err().unwrap()
            } else {
                reader.record(&id("page_query")).err().unwrap()
            };
            assert_eq!(error.code, ErrorCode::IndexCorrupt);
        }
    }

    #[test]
    fn bounded_diagnostics_select_only_requested_paths_and_fail_on_excess() {
        let (_temp, _root, catalog) = fixture();
        let db = database(&catalog);
        db.execute_batch("DELETE FROM diagnostics").unwrap();
        for (value, details) in [("page.md", "{\"fact\":1}"), ("unselected.md", "{")] {
            db.execute(
                "INSERT INTO diagnostics VALUES(1,?1,NULL,?2,?3)",
                params![value, ErrorCode::RecordInvalid.to_string(), details],
            )
            .unwrap();
        }
        let paths = BTreeSet::from([path("page.md")]);
        let reader = defaults(&catalog);
        let diagnostics = reader.diagnostics(&paths).unwrap();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].details, serde_json::json!({"fact": 1}));
        assert_eq!(reader.usage().rows, 1);
        db.execute(
            "INSERT INTO diagnostics VALUES(1,'page.md',NULL,?1,'{}')",
            [ErrorCode::RecordInvalid.to_string()],
        )
        .unwrap();
        let limited = catalog
            .query_snapshot(QueryReadLimits {
                max_rows: 1,
                ..QueryReadLimits::default()
            })
            .unwrap();
        assert_eq!(
            limited.diagnostics(&paths).err().unwrap().code,
            ErrorCode::BudgetExceeded
        );
        assert_eq!(limited.usage().rows, 1);
    }

    #[test]
    fn legacy_missing_diagnostics_index_never_falls_back_to_full_scan() {
        let (_temp, _root, catalog) = fixture();
        database(&catalog)
            .execute_batch("DROP INDEX diagnostic_paths")
            .unwrap();
        let reader = defaults(&catalog);
        assert!(reader.document(&path("page.md")).unwrap().is_some());
        assert!(reader.diagnostics(&BTreeSet::new()).unwrap().is_empty());
        let error = reader
            .diagnostics(&BTreeSet::from([path("page.md")]))
            .err()
            .unwrap();
        assert_eq!(error.code, ErrorCode::CapabilityUnavailable);
        assert_eq!(error.hint.as_deref(), Some("run index rebuild"));
    }

    #[test]
    fn oversized_and_malformed_selected_diagnostics_are_metered() {
        for details in ["{".to_owned(), "x".repeat(4096)] {
            let (_temp, _root, catalog) = fixture();
            let db = database(&catalog);
            db.execute_batch("DELETE FROM diagnostics").unwrap();
            db.execute(
                "INSERT INTO diagnostics VALUES(1,'page.md',NULL,?1,?2)",
                params![ErrorCode::RecordInvalid.to_string(), details],
            )
            .unwrap();
            let reader = catalog
                .query_snapshot(QueryReadLimits {
                    max_row_bytes: 1024,
                    ..QueryReadLimits::default()
                })
                .unwrap();
            let error = reader
                .diagnostics(&BTreeSet::from([path("page.md")]))
                .err()
                .unwrap();
            assert_eq!(
                error.code,
                if details.len() > 1024 {
                    ErrorCode::BudgetExceeded
                } else {
                    ErrorCode::IndexCorrupt
                }
            );
            assert_eq!(reader.usage().rows, usize::from(details.len() <= 1024));
        }
    }

    #[test]
    fn pinned_query_transaction_retains_previous_published_generation() {
        let (temp, root, catalog) = fixture();
        let held = defaults(&catalog);
        let old = held.snapshot().clone();
        let fingerprint = held.dependency_fingerprint().unwrap();
        fs::write(temp.path().join("page.md"), page("New selected title")).unwrap();
        let writer = WriterPermit::acquire(&root, Duration::from_millis(200)).unwrap();
        let published = catalog.sync(&writer).unwrap();
        assert!(published.snapshot.generation > old.generation);
        assert_eq!(held.snapshot(), &old);
        assert_eq!(
            held.document(&path("page.md")).unwrap().unwrap().title,
            "Old selected title"
        );
        assert_eq!(
            held.record(&id("page_query"))
                .unwrap()
                .unwrap()
                .record
                .title(),
            "Old selected title"
        );
        assert_eq!(held.dependency_fingerprint().unwrap(), fingerprint);
        let current = defaults(&catalog);
        assert_eq!(
            current.document(&path("page.md")).unwrap().unwrap().title,
            "New selected title"
        );
        assert_ne!(current.dependency_fingerprint().unwrap(), fingerprint);
    }

    #[test]
    fn scope_fingerprint_commits_caller_vault_and_header_only() {
        let (_temp, root, catalog) = fixture();
        let reader = defaults(&catalog);
        let expected = Blake3Hash::digest(
            serde_json::to_vec(&("indexed_evidence", id("vault_query"), reader.snapshot()))
                .unwrap(),
        );
        assert_eq!(reader.dependency_fingerprint().unwrap(), expected);
        let other = Catalog::new(VaultFs::new(root), id("other_vault"));
        assert_ne!(defaults(&other).dependency_fingerprint().unwrap(), expected);
        // Caller identity is deliberately not enough to authorize any citation.
        assert_eq!(
            defaults(&other).verification(),
            &SnapshotVerification::IndexSnapshot
        );
    }
    #[test]
    fn elapsed_query_clock_survives_return_to_rust() {
        let (_temp, _root, catalog) = fixture();
        let reader = catalog
            .cached_query_snapshot(QueryReadLimits {
                max_elapsed_ms: 100,
                ..Default::default()
            })
            .unwrap();
        reader.check_query_budget().unwrap();
        std::thread::sleep(Duration::from_millis(110));
        assert_eq!(
            reader.check_query_budget().unwrap_err().code,
            ErrorCode::BudgetExceeded
        );
        let error = reader
            .connection()
            .query_row("SELECT 42", [], |row| row.get::<_, i64>(0))
            .unwrap_err();
        assert_eq!(sql::sql_error(error).code, ErrorCode::BudgetExceeded);
    }

    #[test]
    fn sql_work_budget_interrupts_even_when_query_would_return_no_rows() {
        let connection = Connection::open_in_memory().unwrap();
        configure_query(
            &connection,
            &QueryReadLimits {
                max_vm_steps: 10_000,
                ..Default::default()
            },
        )
        .unwrap();
        let error = connection.query_row(
            "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<1000000) SELECT x FROM n WHERE x<0",
            [], |r| r.get::<_, i64>(0),
        ).unwrap_err();
        assert_eq!(sql::sql_error(error).code, ErrorCode::BudgetExceeded);
    }

    #[test]
    fn sql_work_budget_is_cumulative_across_small_statements() {
        let connection = Connection::open_in_memory().unwrap();
        configure_query(
            &connection,
            &QueryReadLimits {
                max_vm_steps: 5000,
                ..Default::default()
            },
        )
        .unwrap();
        let mut interrupted = false;
        for _ in 0..5000 {
            match connection.query_row("SELECT 42", [], |r| r.get::<_, i64>(0)) {
                Ok(42) => (),
                Err(error) => {
                    assert_eq!(sql::sql_error(error).code, ErrorCode::BudgetExceeded);
                    interrupted = true;
                    break;
                }
                other => panic!("unexpected result: {other:?}"),
            }
        }
        assert!(
            interrupted,
            "short queries must not reset the connection work allowance"
        );
    }

    #[test]
    fn sql_deadline_and_value_size_refuse_before_rust_decode() {
        let connection = Connection::open_in_memory().unwrap();
        configure_query(
            &connection,
            &QueryReadLimits {
                max_elapsed_ms: 1,
                ..Default::default()
            },
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(3));
        let error = connection.query_row(
            "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<1000000) SELECT x FROM n WHERE x<0", [], |r| r.get::<_, i64>(0),
        ).unwrap_err();
        assert_eq!(sql::sql_error(error).code, ErrorCode::BudgetExceeded);
        let connection = Connection::open_in_memory().unwrap();
        configure_query(&connection, &QueryReadLimits::default()).unwrap();
        let error = connection
            .query_row("SELECT length(zeroblob(33554433))", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap_err();
        assert_eq!(sql::sql_error(error).code, ErrorCode::BudgetExceeded);
    }
}

impl QuerySnapshot {
    /// Select current assertions through indexed Entity endpoint keys before
    /// hydrating records. Both directional streams share the caller's cap.
    pub(crate) fn incident_assertion_ids(
        &self,
        entity: &RecordId,
        filters: &crate::retrieval::SearchFilters,
        limit: usize,
    ) -> Result<BoundedIncidentIds> {
        use crate::retrieval::filters::{bind, catalog_sql, normalize};
        use rusqlite::{params_from_iter, types::Value};
        if limit == 0 || limit > 16 {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "incident selection permits 1–16 assertions",
            ));
        }
        let filters = normalize(filters)?;
        self.require_fact_layout()?;
        self.fact_requests(2, true)?;
        let mut common = filters.clone();
        common.source_ids.clear();
        let mut values = vec![Value::Text(entity.as_str().into()), Value::Null];
        let filter_sql = catalog_sql(&common, &mut values, true);
        // A source scope requires supporting evidence, rather than merely a
        // declared Source or a contrary evidence association.
        let source_sql = if filters.source_ids.is_empty() {
            "1".into()
        } else {
            let ids = filters
                .source_ids
                .iter()
                .map(|id| bind(&mut values, Value::Text(id.as_str().into())))
                .collect::<Vec<_>>()
                .join(",");
            format!(
                "EXISTS(SELECT 1 FROM source_evidence s INDEXED BY source_assertions \
                JOIN records er ON er.id=s.evidence_id \
                WHERE s.source_id IN ({ids}) AND s.assertion_id=e.owner_id \
                AND er.kind='evidence' AND er.eligibility='current' AND er.authored_status='active' \
                AND json_extract(er.row_json,'$.record.wiki_stance')='supports')"
            )
        };
        let cap = bind(&mut values, Value::Integer((limit + 1) as i64));
        for value in &values[2..] {
            if let Value::Text(text) = value {
                self.reserve_fact_input(text.len())?;
            }
        }
        let query = format!(
            "SELECT e.owner_id,e.target_id,e.role_json \
            FROM semantic_edges e INDEXED BY semantic_dependents \
            CROSS JOIN records r ON r.id=e.owner_id \
            CROSS JOIN documents d INDEXED BY document_record_ids ON d.record_id=e.owner_id \
            WHERE e.target_id=?1 AND e.role_json=?2 AND r.kind='assertion' \
            AND r.eligibility='current' AND r.authored_status='accepted' \
            AND ({filter_sql}) AND ({source_sql}) ORDER BY e.owner_id LIMIT {cap}"
        );
        let mut statement = self.connection.prepare(&query).map_err(sql::sql_error)?;
        let mut selected = BTreeSet::new();
        let mut has_more = false;
        for field in ["wiki_subject_id", "wiki_object_id"] {
            let role = super::eligibility_facts::EligibilityRole::TypedReference {
                field: field.into(),
            };
            let encoded = sql::json(&role)?;
            self.reserve_fact_input(entity.as_str().len() + encoded.len())?;
            values[1] = Value::Text(encoded.clone());
            #[cfg(test)]
            if super::query_diagnostics::active() {
                let before = super::query_diagnostics::vm_count();
                let mut explanation = self
                    .connection
                    .prepare(&format!("EXPLAIN QUERY PLAN {query}"))
                    .map_err(sql::sql_error)?;
                let details = explanation
                    .query_map(params_from_iter(values.iter()), |row| {
                        row.get::<_, String>(3)
                    })
                    .map_err(sql::sql_error)?
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(sql::sql_error)?;
                super::query_diagnostics::plan(
                    &query,
                    details,
                    super::query_diagnostics::vm_count() - before,
                );
            }
            let mut rows = statement
                .query(params_from_iter(values.iter()))
                .map_err(sql::sql_error)?;
            let mut count = 0;
            while let Some(row) = rows.next().map_err(sql::sql_error)? {
                self.reserve_refresh_row(row, 3)?;
                if utf8(text_bytes(row, 1)?)? != entity.as_str()
                    || utf8(text_bytes(row, 2)?)? != encoded
                {
                    return Err(corrupt(
                        "incident endpoint key differs from requested scope",
                    ));
                }
                selected.insert(
                    RecordId::new(utf8(text_bytes(row, 0)?)?).map_err(|e| corrupt(e.message))?,
                );
                count += 1;
            }
            has_more |= count > limit;
        }
        has_more |= selected.len() > limit;
        Ok(BoundedIncidentIds {
            ids: selected.into_iter().take(limit).collect(),
            has_more,
        })
    }

    pub(crate) fn affected_assertion_navigation(
        &self,
        keys: &[super::link_facts::MatchKey],
    ) -> Result<Vec<RecordId>> {
        self.fact_requests(keys.len(), false)?;
        self.require_fact_layout()?;
        let mut statement=self.connection.prepare("SELECT kind,value,assertion_id FROM assertion_navigation_keys WHERE kind=?1 AND value=?2 ORDER BY assertion_id").map_err(sql::sql_error)?;
        let mut owners = BTreeSet::new();
        for key in keys {
            self.reserve_fact_input(key.value.len())?;
            let mut rows = statement
                .query(params![fact_key_name(key.kind), key.value])
                .map_err(sql::sql_error)?;
            while let Some(row) = rows.next().map_err(sql::sql_error)? {
                self.reserve_refresh_row(row, 3)?;
                if utf8(text_bytes(row, 0)?)? != fact_key_name(key.kind)
                    || utf8(text_bytes(row, 1)?)? != key.value
                {
                    return Err(corrupt(
                        "assertion navigation key differs from requested scope",
                    ));
                }
                owners.insert(
                    RecordId::new(utf8(text_bytes(row, 2)?)?).map_err(|e| corrupt(e.message))?,
                );
            }
        }
        Ok(owners.into_iter().collect())
    }
}

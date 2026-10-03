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
    Blake3Hash, ErrorCode, ReadSnapshot, RecordId, Result, VaultRelativePath, WikiError,
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
    limits: QueryReadLimits,
    operation_authority: Option<Authority>,
}

impl Catalog {
    pub(crate) fn query_snapshot(&self, limits: QueryReadLimits) -> Result<QuerySnapshot> {
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
                let connection = Connection::open_with_flags(
                    path,
                    OpenFlags::SQLITE_OPEN_READ_ONLY
                        | OpenFlags::SQLITE_OPEN_NO_MUTEX
                        | OpenFlags::SQLITE_OPEN_NOFOLLOW,
                )
                .map_err(sql::sql_error)?;
                configure_query(&connection, &limits)?;
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
                selected_header = Some(header.snapshot);
                Ok(connection)
            },
        )? {
            let snapshot = selected_header.expect("successful selection reads its header");
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
                limits,
                operation_authority,
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
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(sql::sql_error)?;
        configure_query(&connection, &limits)?;
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
            limits,
            operation_authority: None,
        })
    }
}

fn configure_query(connection: &Connection, limits: &QueryReadLimits) -> Result<()> {
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
    Ok(())
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

    pub(crate) fn pending_operation_at_start(&self) -> Option<RecordId> {
        self.operation_authority
            .as_ref()
            .and_then(Authority::active)
            .map(|active| active.change.change_id.clone())
    }

    pub(crate) fn usage(&self) -> QueryReadUsage {
        self.usage.get()
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
        serde_json::from_slice(bytes).map_err(|error| corrupt(error.to_string()))
    }
}

impl QueryCatalog for QuerySnapshot {
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
        "indexed_evidence"
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
                })?)
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
    use std::{fs, time::Duration};

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
        let projection = scan::project_with_sink(&catalog.fs, &input, false, &mut builder).unwrap();
        let completed = builder.finish(&projection).unwrap();
        selector::publish(
            &catalog.fs,
            &writer,
            &completed.identity.selection,
            Duration::from_secs(1),
        )
        .unwrap();
        completed
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
            Connection::open(completed.path)
                .unwrap()
                .execute_batch(mutation)
                .unwrap();
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
        Connection::open(completed.path)
            .unwrap()
            .execute_batch(
                "DROP INDEX diagnostic_paths; CREATE INDEX diagnostic_paths ON diagnostics(code)",
            )
            .unwrap();
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
    fn selected_v2_read_only_catalog_directory_is_unchanged() {
        use std::os::unix::fs::PermissionsExt;
        let (temp, _root, catalog) = unsynced();
        let completed = publish_normalized(&catalog, 1);
        let directory = completed.path.parent().unwrap();
        let before: BTreeSet<_> = fs::read_dir(directory)
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                (
                    path.clone(),
                    fs::read(&path).unwrap(),
                    fs::metadata(&path).unwrap().modified().unwrap(),
                )
            })
            .collect();
        let permissions = fs::metadata(directory).unwrap().permissions();
        fs::set_permissions(directory, fs::Permissions::from_mode(0o500)).unwrap();
        let reader = defaults(&catalog);
        reader.document(&path("page.md")).unwrap().unwrap();
        reader.record(&id("page_query")).unwrap().unwrap();
        drop(reader);
        fs::set_permissions(directory, permissions).unwrap();
        let after: BTreeSet<_> = fs::read_dir(directory)
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                (
                    path.clone(),
                    fs::read(&path).unwrap(),
                    fs::metadata(&path).unwrap().modified().unwrap(),
                )
            })
            .collect();
        assert_eq!(before, after);
        assert!(!temp.path().join(".wiki/cache/index.sqlite").exists());
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

//! One connection/read transaction pins ordinary and FTS rows to one generation.
use super::{scan, sql, types::*};
use crate::{
    domain::{ErrorCode, ReadSnapshot, RecordKind, Result, WikiError},
    vault::WriterPermit,
};
use rusqlite::params;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

impl Catalog {
    pub fn index_snapshot(&self) -> Result<ReaderSnapshot> {
        let path = self.cache_path()?;
        if !path.exists() {
            return Err(WikiError::new(
                ErrorCode::OfflineUnavailable,
                "catalog cache is absent",
            ));
        }
        let c = sql::open(&path, self.options.busy_timeout_ms, false)?;
        sql::validate(&c)?;
        c.execute_batch("BEGIN DEFERRED").map_err(sql::sql_error)?;
        // The first read establishes SQLite's snapshot, including both FTS virtual tables.
        let (generation,parser,manifest,serialized):(i64,String,String,String)=c.query_row("SELECT g.gen,g.parser_hash,g.manifest_hash,g.projection_json FROM generations g JOIN index_meta m ON g.gen=m.published_gen WHERE g.state='complete'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).map_err(|e|if matches!(e,rusqlite::Error::QueryReturnedNoRows){WikiError::new(ErrorCode::OfflineUnavailable,"catalog has no published generation")}else{sql::sql_error(e)})?;
        let projection: CatalogProjection = serde_json::from_str(&serialized)
            .map_err(|e| WikiError::new(ErrorCode::IndexCorrupt, e.to_string()))?;
        let snapshot = ReadSnapshot {
            generation: u64::try_from(generation)
                .map_err(|_| WikiError::new(ErrorCode::IndexCorrupt, "negative generation"))?,
            parser_fingerprint: crate::domain::Blake3Hash::new(parser)?,
            control_manifest: crate::domain::Blake3Hash::new(manifest)?,
        };
        if projection.vault_id != self.vault_id
            || snapshot.parser_fingerprint != projection.parser_fingerprint
            || snapshot.control_manifest != projection.control_manifest
        {
            return Err(WikiError::new(
                ErrorCode::IndexCorrupt,
                "catalog projection binding mismatch",
            ));
        }
        super::integrity::validate_projection(&c, generation, &projection)?;
        Ok(ReaderSnapshot {
            connection: c,
            snapshot,
            projection,
            verification: SnapshotVerification::IndexSnapshot,
        })
    }
    pub fn verified_snapshot(&self, writer: Option<&WriterPermit>) -> Result<ReaderSnapshot> {
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
}
impl ReaderSnapshot {
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

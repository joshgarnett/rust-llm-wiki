//! Bounded diagnostics from an existing publication, without canonical scanning.
use super::{
    Catalog, CatalogDiagnostic,
    query::QuerySnapshot,
    query_types::{QueryCatalog, QueryReadLimits},
    sql,
};
use crate::domain::*;
use rusqlite::{Row, params, types::ValueRef};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone)]
pub(crate) struct DiagnosticPageRequest {
    pub limit: usize,
    pub cursor: Option<String>,
    pub max_bytes: usize,
}
impl Default for DiagnosticPageRequest {
    fn default() -> Self {
        Self {
            limit: 50,
            cursor: None,
            max_bytes: 65_536,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PublishedHealth {
    pub index_generation: u64,
    pub has_issues: bool,
    pub diagnostic_count: u64,
    pub excluded_document_count: u64,
    pub inspection_only_document_count: u64,
    pub diagnostics: Vec<CatalogDiagnostic>,
    pub diagnostics_truncated: bool,
    pub continuation: Option<String>,
    pub omitted_detail_rows: usize,
}
#[derive(Debug, Clone, Serialize)]
pub(crate) struct KnownHealth {
    pub index_generation: u64,
    pub has_issues: bool,
}

fn invalid(message: &str) -> WikiError {
    WikiError::new(ErrorCode::IndexCorrupt, message)
}
fn text<'a>(row: &'a Row<'_>, column: usize) -> Result<&'a [u8]> {
    match row.get_ref(column).map_err(sql::sql_error)? {
        ValueRef::Text(value) => Ok(value),
        _ => Err(invalid("published diagnostic text has wrong type")),
    }
}
fn utf8(bytes: &[u8]) -> Result<&str> {
    std::str::from_utf8(bytes).map_err(|_| invalid("published diagnostic is not UTF-8"))
}

impl Catalog {
    pub(crate) fn published_health(
        &self,
        request: DiagnosticPageRequest,
    ) -> Result<PublishedHealth> {
        if request.limit == 0
            || request.limit > 100
            || request.max_bytes < 16_384
            || request.max_bytes > 1_048_576
        {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "diagnostic page requires 1..=100 rows and 16384..=1048576 bytes",
            ));
        }
        let reader = self.cached_query_snapshot(QueryReadLimits::default())?;
        let health = reader.published_health(&request)?;
        reader.verify_operations(self)?;
        Ok(health)
    }
}
impl QuerySnapshot {
    /// One existence probe suffices for a generation-known warning on reads.
    pub(crate) fn known_health(&self) -> Result<KnownHealth> {
        let query = if self.normalized_layout() {
            "SELECT EXISTS(SELECT 1 FROM diagnostics LIMIT 1)"
        } else {
            "SELECT EXISTS(SELECT 1 FROM diagnostics INDEXED BY diagnostic_paths WHERE gen=?1 LIMIT 1)"
        };
        let has_issues = if self.normalized_layout() {
            self.connection()
                .query_row(query, [], |row| row.get::<_, bool>(0))
        } else {
            self.connection()
                .query_row(query, [sql::integer(self.snapshot().generation)?], |row| {
                    row.get::<_, bool>(0)
                })
        }
        .map_err(sql::sql_error)?;
        self.check_query_budget()?;
        Ok(KnownHealth {
            index_generation: self.snapshot().generation,
            has_issues,
        })
    }

    fn published_health(&self, request: &DiagnosticPageRequest) -> Result<PublishedHealth> {
        let fingerprint = self.dependency_fingerprint()?;
        let after = decode_cursor(request.cursor.as_deref(), &fingerprint)?;
        let generation = sql::integer(self.snapshot().generation)?;
        let counts = if self.normalized_layout() {
            "SELECT (SELECT count(*) FROM diagnostics),(SELECT count(*) FROM documents WHERE eligibility IN ('invalid','unsupported')),(SELECT count(*) FROM documents WHERE record_id IS NULL AND source_id IS NULL AND owner_revision IS NULL AND eligibility='unsupported' AND json_extract(reasons_json,'$[0]')='unadopted_note_inspection_only')"
        } else {
            "SELECT (SELECT count(*) FROM diagnostics WHERE gen=?1),(SELECT count(*) FROM documents WHERE gen=?1 AND eligibility IN ('invalid','unsupported')),(SELECT count(*) FROM documents WHERE gen=?1 AND record_id IS NULL AND source_id IS NULL AND owner_revision IS NULL AND eligibility='unsupported' AND json_extract(row_json,'$.reasons[0]')='unadopted_note_inspection_only')"
        };
        let mut counts_statement = self.connection().prepare(counts).map_err(sql::sql_error)?;
        let mut counts_rows = if self.normalized_layout() {
            counts_statement.query([])
        } else {
            counts_statement.query([generation])
        }
        .map_err(sql::sql_error)?;
        let counts_row = counts_rows
            .next()
            .map_err(sql::sql_error)?
            .ok_or_else(|| invalid("published health counts absent"))?;
        self.reserve_scalar_row(counts_row, 3)?;
        let diagnostic_count = u64::try_from(counts_row.get::<_, i64>(0).map_err(sql::sql_error)?)
            .map_err(|_| invalid("negative diagnostic count"))?;
        let excluded_document_count =
            u64::try_from(counts_row.get::<_, i64>(1).map_err(sql::sql_error)?)
                .map_err(|_| invalid("negative excluded document count"))?;
        let inspection_only_document_count =
            u64::try_from(counts_row.get::<_, i64>(2).map_err(sql::sql_error)?)
                .map_err(|_| invalid("negative inspection-only document count"))?;
        if inspection_only_document_count > excluded_document_count {
            return Err(invalid(
                "inspection-only document count exceeds excluded count",
            ));
        }
        drop(counts_rows);
        drop(counts_statement);
        let query = if self.normalized_layout() {
            "SELECT diagnostic_row,path,record_id,code,details_json FROM diagnostics WHERE diagnostic_row>?1 ORDER BY diagnostic_row LIMIT ?2"
        } else {
            "SELECT rowid,path,record_id,code,details_json FROM diagnostics WHERE gen=?3 AND rowid>?1 ORDER BY rowid LIMIT ?2"
        };
        let mut statement = self.connection().prepare(query).map_err(sql::sql_error)?;
        let mut rows = if self.normalized_layout() {
            statement.query(params![after, request.limit as i64 + 1])
        } else {
            statement.query(params![after, request.limit as i64 + 1, generation])
        }
        .map_err(sql::sql_error)?;
        let mut diagnostics = Vec::new();
        let mut bytes = 0usize;
        let mut last = after;
        let mut truncated = false;
        let mut omitted_detail_rows = 0usize;
        while let Some(row) = rows.next().map_err(sql::sql_error)? {
            self.reserve_scalar_row(row, 5)?;
            if diagnostics.len() == request.limit {
                truncated = true;
                break;
            }
            let position = row.get::<_, i64>(0).map_err(sql::sql_error)?;
            if position <= last {
                return Err(invalid("diagnostic continuation ordering differs"));
            }
            let raw_path = text(row, 1)?;
            if raw_path.len() > 4096 {
                return Err(invalid("published diagnostic path exceeds 4096 bytes"));
            }
            let path = VaultRelativePath::new(utf8(raw_path)?)
                .map_err(|_| invalid("invalid published diagnostic path"))?;
            let record_id = match row.get_ref(2).map_err(sql::sql_error)? {
                ValueRef::Null => None,
                ValueRef::Text(bytes) => Some(
                    RecordId::new(utf8(bytes)?)
                        .map_err(|_| invalid("invalid published diagnostic record ID"))?,
                ),
                _ => return Err(invalid("published diagnostic record ID has wrong type")),
            };
            let code: ErrorCode =
                serde_json::from_value(Value::String(utf8(text(row, 3)?)?.to_owned()))
                    .map_err(|_| invalid("invalid published diagnostic code"))?;
            let details_bytes = text(row, 4)?;
            let details_omitted = details_bytes.len() > 8192;
            let details = if details_omitted {
                serde_json::json!({"details_omitted":true,"original_details_bytes":details_bytes.len(),"next_action":"inspect the physical file; continue diagnostics for other claimed paths"})
            } else {
                crate::changes::prepare::strict_json(details_bytes)
                    .map_err(|_| invalid("invalid published diagnostic JSON"))?
            };
            let diagnostic = CatalogDiagnostic {
                path,
                record_id,
                code,
                details,
            };
            let size = serde_json::to_vec(&diagnostic)
                .map_err(|error| WikiError::new(ErrorCode::Internal, error.to_string()))?
                .len();
            if bytes.saturating_add(size) > request.max_bytes {
                if diagnostics.is_empty() {
                    let mut error = WikiError::new(
                        ErrorCode::BudgetExceeded,
                        "first diagnostic exceeds the page byte ceiling",
                    );
                    error.details = serde_json::json!({"path":diagnostic.path,"minimum_bytes":size,"ceiling_bytes":request.max_bytes});
                    error.hint = Some(
                        "Request a larger diagnostic byte ceiling or inspect the physical path"
                            .into(),
                    );
                    return Err(error);
                }
                truncated = true;
                break;
            }
            bytes += size;
            last = position;
            omitted_detail_rows += usize::from(details_omitted);
            diagnostics.push(diagnostic);
        }
        self.check_query_budget()?;
        Ok(PublishedHealth {
            index_generation: self.snapshot().generation,
            has_issues: diagnostic_count != 0,
            diagnostic_count,
            excluded_document_count,
            inspection_only_document_count,
            diagnostics,
            diagnostics_truncated: truncated,
            continuation: truncated.then(|| encode_cursor(&fingerprint, last)),
            omitted_detail_rows,
        })
    }
}
fn encode_cursor(fingerprint: &Blake3Hash, after: i64) -> String {
    format!(
        "health1:{}:{after}",
        fingerprint
            .as_str()
            .strip_prefix("blake3:")
            .expect("validated hash")
    )
}
fn decode_cursor(cursor: Option<&str>, expected: &Blake3Hash) -> Result<i64> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    if cursor.len() > 100 {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "diagnostic cursor is invalid",
        ));
    }
    let mut parts = cursor.split(':');
    let version = parts.next();
    let hash = parts.next();
    let after = parts.next().and_then(|value| value.parse::<i64>().ok());
    if version != Some("health1")
        || hash.is_none()
        || after.is_none()
        || after.is_some_and(|value| value < 0)
        || parts.next().is_some()
    {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "diagnostic cursor is invalid",
        ));
    }
    let fingerprint = Blake3Hash::new(format!("blake3:{}", hash.expect("checked hash")))
        .map_err(|_| WikiError::new(ErrorCode::Usage, "diagnostic cursor hash is invalid"))?;
    if &fingerprint != expected {
        return Err(WikiError::new(
            ErrorCode::CursorStale,
            "diagnostic cursor belongs to another publication; restart diagnostics",
        ));
    }
    let after = after.expect("checked position");
    if encode_cursor(&fingerprint, after) != cursor {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "diagnostic cursor is not canonical",
        ));
    }
    Ok(after)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diagnostic_continuations_bind_publication_and_refuse_ambiguous_spelling() {
        let first = Blake3Hash::digest(b"first physical publication");
        let replacement = Blake3Hash::digest(b"replacement publication");
        assert_eq!(decode_cursor(None, &first).unwrap(), 0);
        let cursor = encode_cursor(&first, 123);
        assert_eq!(decode_cursor(Some(&cursor), &first).unwrap(), 123);
        assert_eq!(
            decode_cursor(Some(&cursor), &replacement).unwrap_err().code,
            ErrorCode::CursorStale
        );
        for cursor in [
            "invalid".to_owned(),
            format!(
                "health1:{}:-1",
                first.as_str().trim_start_matches("blake3:")
            ),
            format!(
                "health1:{}:0123",
                first.as_str().trim_start_matches("blake3:")
            ),
        ] {
            assert_eq!(
                decode_cursor(Some(&cursor), &first).unwrap_err().code,
                ErrorCode::Usage
            );
        }
    }

    #[test]
    fn persistent_issue_pages_are_complete_bounded_and_stale_after_publication() {
        use crate::vault::{VaultFs, VaultRoot, WriterPermit};
        use std::{collections::BTreeSet, fs, time::Duration};
        for normalized in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().canonicalize().unwrap();
            fs::write(path.join("WIKI.md"),b"---\nwiki_schema: \"1\"\nwiki_id: Vault.Health\nwiki_kind: vault\ntitle: Health\n---\n").unwrap();
            fs::write(path.join("plain.md"), b"ordinary editor note").unwrap();
            for i in 0..12 {
                fs::write(path.join(format!("bad-{i:02}.md")),format!("---\nwiki_schema: \"1\"\nwiki_id: Broken.{i}\nwiki_kind: page\ntitle: [wrong, type]\n---\ninvalid author metadata\n")).unwrap();
            }
            let root = VaultRoot::explicit(&path).unwrap();
            let catalog = Catalog::new(
                VaultFs::new(root.clone()),
                RecordId::new("Vault.Health").unwrap(),
            );
            let writer = WriterPermit::acquire(&root, Duration::from_millis(1000)).unwrap();
            if normalized {
                catalog.rebuild_normalized(&writer).unwrap();
            } else {
                catalog.sync(&writer).unwrap();
            }
            let mut request = DiagnosticPageRequest {
                limit: 3,
                ..Default::default()
            };
            let mut observed = BTreeSet::new();
            let first = catalog.published_health(request.clone()).unwrap();
            assert!(first.has_issues);
            assert_eq!(first.diagnostic_count, 12);
            assert_eq!(first.inspection_only_document_count, 1);
            assert_eq!(first.excluded_document_count, 13);
            let old_cursor = first.continuation.clone().unwrap();
            loop {
                let page = catalog.published_health(request.clone()).unwrap();
                assert!(page.diagnostics.len() <= 3);
                for diagnostic in page.diagnostics {
                    assert!(observed.insert(diagnostic.path));
                }
                let Some(cursor) = page.continuation else {
                    break;
                };
                request.cursor = Some(cursor);
            }
            assert_eq!(observed.len(), 12);
            let reader = catalog
                .cached_query_snapshot(QueryReadLimits::default())
                .unwrap();
            assert!(reader.known_health().unwrap().has_issues);
            drop(reader);
            fs::write(
                path.join("another.md"),
                b"---\nwiki_schema: \"1\"\nwiki_kind: page\n---\nmissing ID\n",
            )
            .unwrap();
            if normalized {
                catalog.sync_normalized(&writer).unwrap();
            } else {
                catalog.sync(&writer).unwrap();
            }
            assert_eq!(
                catalog
                    .published_health(DiagnosticPageRequest {
                        cursor: Some(old_cursor),
                        ..Default::default()
                    })
                    .unwrap_err()
                    .code,
                ErrorCode::CursorStale
            );
        }
    }
}

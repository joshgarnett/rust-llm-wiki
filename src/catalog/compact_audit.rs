//! Complete native FTS proof against an owned, contentless disk reference.
//! The caller owns source admission/pinning, scratch lifetime and ordinary-row
//! equality. Scratch failure is terminal; it must never be reused or published.
use super::{
    full_check_types::CheckBudget,
    normalized_audit,
    types::{DocumentRow, GraphRow},
};
use crate::domain::{ErrorCode, Result};
use rusqlite::{Connection, Row, params, types::ValueRef};
use serde::Serialize;

const SCRATCH_SCHEMA: &str = "
CREATE VIRTUAL TABLE documents_fts USING fts5(title,aliases,headings,tags,body,
 content='',detail=full,columnsize=1,tokenize='unicode61 remove_diacritics 2');
CREATE VIRTUAL TABLE documents_vocab USING fts5vocab(documents_fts,'instance');
CREATE VIRTUAL TABLE graph_fts USING fts5(name,aliases,endpoints,predicate,qualifiers,description,
 target_kind UNINDEXED,target_id UNINDEXED,content='',detail=full,columnsize=1,
 tokenize='unicode61 remove_diacritics 2');
CREATE VIRTUAL TABLE graph_vocab USING fts5vocab(graph_fts,'instance');";
const DOCUMENT_COLUMNS: &[&str] = &["title", "aliases", "headings", "tags", "body"];
const GRAPH_COLUMNS: &[&str] = &[
    "name",
    "aliases",
    "endpoints",
    "predicate",
    "qualifiers",
    "description",
];

#[derive(Debug, Default, Serialize)]
pub(crate) struct CompactStats {
    pub source_database_bytes: u64,
    pub scratch_database_bytes: u64,
    pub documents: u64,
    pub graph_rows: u64,
    pub document_postings: u64,
    pub graph_postings: u64,
    pub document_docsize_rows: u64,
    pub graph_docsize_rows: u64,
}

pub(crate) struct CompactAudit<'a> {
    source: &'a Connection,
    scratch: &'a Connection,
    budget: CheckBudget,
    stats: CompactStats,
}
impl<'a> CompactAudit<'a> {
    pub(crate) fn new(
        source: &'a Connection,
        scratch: &'a Connection,
        budget: CheckBudget,
    ) -> Result<Self> {
        budget.guard()?;
        if source.is_autocommit()
            || !source
                .is_readonly("main")
                .map_err(|e| budget.sql_error(e))?
        {
            return Err(budget.fail(
                ErrorCode::IndexCorrupt,
                "compact audit requires a pinned read-only source",
            ));
        }
        let source_bytes = database_bytes(source, &budget)?;
        if source_bytes > budget.limits().max_source_bytes {
            return Err(budget.fail(
                ErrorCode::BudgetExceeded,
                "compact audit source exceeds byte limit",
            ));
        }
        normalized_audit::validate_schema(source).map_err(|e| budget.fail(e.code, e.message))?;
        native_integrity(source, &budget)?;
        budget.guard()?;
        if !scratch.is_autocommit()
            || scratch
                .is_readonly("main")
                .map_err(|e| budget.sql_error(e))?
        {
            return Err(budget.fail(
                ErrorCode::IndexCorrupt,
                "compact audit requires fresh writable scratch",
            ));
        }
        budget.configure_sql(scratch)?;
        let occupied: bool = scratch
            .query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema)", [], |r| {
                r.get(0)
            })
            .map_err(|e| budget.sql_error(e))?;
        if occupied {
            return Err(budget.fail(
                ErrorCode::IndexCorrupt,
                "compact audit scratch is not empty",
            ));
        }
        scratch
            .execute_batch(
                "PRAGMA page_size=4096; PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF;",
            )
            .map_err(|e| budget.sql_error(e))?;
        let max_pages = (budget.limits().max_scratch_bytes / 4096) as i64;
        let actual: i64 = scratch
            .pragma_update_and_check(None, "max_page_count", max_pages, |r| r.get(0))
            .map_err(|e| budget.sql_error(e))?;
        let page_size: i64 = scratch
            .pragma_query_value(None, "page_size", |r| r.get(0))
            .map_err(|e| budget.sql_error(e))?;
        if actual != max_pages || page_size != 4096 {
            return Err(budget.fail(
                ErrorCode::IndexCorrupt,
                "compact audit scratch geometry differs from admitted cap",
            ));
        }
        scratch
            .execute_batch("BEGIN")
            .map_err(|e| budget.sql_error(e))?;
        scratch
            .execute_batch(SCRATCH_SCHEMA)
            .map_err(|e| budget.sql_error(e))?;
        budget.guard()?;
        Ok(Self {
            source,
            scratch,
            budget,
            stats: CompactStats {
                source_database_bytes: source_bytes,
                ..Default::default()
            },
        })
    }

    pub(crate) fn document(&mut self, actual_rowid: i64, row: &DocumentRow) -> Result<()> {
        self.budget.guard()?;
        let alias_bytes = joined_bytes(&row.aliases, &self.budget)?;
        let tag_bytes = joined_bytes(&row.tags, &self.budget)?;
        admit_text_bytes(
            &self.budget,
            &[
                row.title.len(),
                alias_bytes,
                row.headings.len(),
                tag_bytes,
                row.body.len(),
            ],
        )?;
        self.scratch.execute("INSERT INTO documents_fts(rowid,title,aliases,headings,tags,body) VALUES(?1,?2,?3,?4,?5,?6)",
            params![actual_rowid, row.title, row.aliases.join(" "), row.headings, row.tags.join(" "), row.body])
            .map_err(|e| self.budget.sql_error(e))?;
        self.stats.documents += 1;
        self.budget.guard()
    }

    pub(crate) fn graph(&mut self, actual_rowid: i64, row: &GraphRow) -> Result<()> {
        self.budget.guard()?;
        let alias_bytes = joined_bytes(&row.aliases, &self.budget)?;
        let kind = row.target_kind.as_str();
        admit_text_bytes(
            &self.budget,
            &[
                row.name.len(),
                alias_bytes,
                row.endpoints.len(),
                row.predicate.len(),
                row.qualifiers.len(),
                row.description.len(),
                kind.len(),
                row.target_id.as_str().len(),
            ],
        )?;
        self.scratch.execute("INSERT INTO graph_fts(rowid,name,aliases,endpoints,predicate,qualifiers,description,target_kind,target_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![actual_rowid, row.name, row.aliases.join(" "), row.endpoints, row.predicate, row.qualifiers,
                row.description, kind, row.target_id.as_str()]).map_err(|e| self.budget.sql_error(e))?;
        self.stats.graph_rows += 1;
        self.budget.guard()
    }

    pub(crate) fn finish(mut self) -> Result<CompactStats> {
        self.budget.guard()?;
        // Flush buffered FTS segments and average counters before any proof read.
        self.scratch
            .execute_batch("COMMIT")
            .map_err(|e| self.budget.sql_error(e))?;
        self.stats.scratch_database_bytes = database_bytes(self.scratch, &self.budget)?;
        if self.stats.scratch_database_bytes > self.budget.limits().max_scratch_bytes {
            return Err(self.budget.fail(
                ErrorCode::BudgetExceeded,
                "compact audit scratch exceeds byte limit",
            ));
        }
        self.stats.document_postings = compare_postings(
            self.source,
            self.scratch,
            "documents_vocab",
            DOCUMENT_COLUMNS,
            &self.budget,
        )?;
        self.stats.graph_postings = compare_postings(
            self.source,
            self.scratch,
            "graph_vocab",
            GRAPH_COLUMNS,
            &self.budget,
        )?;
        self.stats.document_docsize_rows = compare_docsize(
            self.source,
            self.scratch,
            "documents_fts_docsize",
            &self.budget,
        )?;
        self.stats.graph_docsize_rows =
            compare_docsize(self.source, self.scratch, "graph_fts_docsize", &self.budget)?;
        compare_totals(
            self.source,
            self.scratch,
            "documents_fts_data",
            5,
            self.stats.document_docsize_rows == 0,
            &self.budget,
        )?;
        compare_totals(
            self.source,
            self.scratch,
            "graph_fts_data",
            8,
            self.stats.graph_docsize_rows == 0,
            &self.budget,
        )?;
        self.budget.guard()?;
        Ok(self.stats)
    }
}

fn admit_text_bytes(budget: &CheckBudget, lengths: &[usize]) -> Result<()> {
    let total = lengths
        .iter()
        .try_fold(0u64, |n, &len| n.checked_add(len as u64))
        .ok_or_else(|| {
            budget.fail(
                ErrorCode::BudgetExceeded,
                "compact audit text size overflow",
            )
        })?;
    if total > budget.limits().max_row_bytes {
        return Err(budget.fail(
            ErrorCode::BudgetExceeded,
            "compact audit indexed text exceeds row limit",
        ));
    }
    Ok(())
}
fn joined_bytes(values: &[String], budget: &CheckBudget) -> Result<usize> {
    let mut total = values.len().saturating_sub(1);
    for (i, value) in values.iter().enumerate() {
        if i % 1000 == 0 {
            budget.guard()?;
        }
        total = total.checked_add(value.len()).ok_or_else(|| {
            budget.fail(
                ErrorCode::BudgetExceeded,
                "compact audit joined text size overflow",
            )
        })?;
        if total as u64 > budget.limits().max_row_bytes {
            return Err(budget.fail(
                ErrorCode::BudgetExceeded,
                "compact audit joined text exceeds row limit",
            ));
        }
    }
    Ok(total)
}
fn database_bytes(c: &Connection, budget: &CheckBudget) -> Result<u64> {
    budget.guard()?;
    let pages: i64 = c
        .pragma_query_value(None, "page_count", |r| r.get(0))
        .map_err(|e| budget.sql_error(e))?;
    let size: i64 = c
        .pragma_query_value(None, "page_size", |r| r.get(0))
        .map_err(|e| budget.sql_error(e))?;
    if pages <= 0 || !(512..=65536).contains(&size) || !(size as u64).is_power_of_two() {
        return Err(budget.fail(
            ErrorCode::IndexCorrupt,
            "compact audit invalid page geometry",
        ));
    }
    (pages as u64).checked_mul(size as u64).ok_or_else(|| {
        budget.fail(
            ErrorCode::BudgetExceeded,
            "compact audit database byte overflow",
        )
    })
}
fn native_integrity(c: &Connection, budget: &CheckBudget) -> Result<()> {
    require_plan(c, "PRAGMA main.integrity_check(1)", budget, true)?;
    require_plan(c, "PRAGMA main.foreign_key_check", budget, true)?;
    let mut stmt = c
        .prepare("PRAGMA main.integrity_check(1)")
        .map_err(|e| budget.sql_error(e))?;
    let mut rows = stmt.query([]).map_err(|e| budget.sql_error(e))?;
    let row = rows
        .next()
        .map_err(|e| budget.sql_error(e))?
        .ok_or_else(|| {
            budget.fail(
                ErrorCode::IndexCorrupt,
                "compact audit native integrity returned no result",
            )
        })?;
    if row.get_ref(0).map_err(|e| budget.sql_error(e))? != ValueRef::Text(b"ok")
        || rows.next().map_err(|e| budget.sql_error(e))?.is_some()
    {
        return Err(budget.fail(
            ErrorCode::IndexCorrupt,
            "compact audit native integrity failed",
        ));
    }
    budget.guard()?;
    let mut stmt = c
        .prepare("PRAGMA main.foreign_key_check")
        .map_err(|e| budget.sql_error(e))?;
    if stmt
        .query([])
        .map_err(|e| budget.sql_error(e))?
        .next()
        .map_err(|e| budget.sql_error(e))?
        .is_some()
    {
        return Err(budget.fail(
            ErrorCode::IndexCorrupt,
            "compact audit foreign key integrity failed",
        ));
    }
    budget.guard()
}

/// Only term ordering is consumed by bundled fts5VocabBestIndexMethod. Requesting
/// doc/col/offset SQL ordering would introduce a corpus-sized external sort.
/// Reject both planner temp-sort reports and actual sorter/ephemeral bytecode.
fn require_stream_plan(c: &Connection, query: &str, budget: &CheckBudget) -> Result<()> {
    require_plan(c, query, budget, false)
}
fn require_plan(c: &Connection, query: &str, budget: &CheckBudget, native: bool) -> Result<()> {
    for (prefix, column) in [("EXPLAIN QUERY PLAN ", 3), ("EXPLAIN ", 1)] {
        let mut stmt = c
            .prepare(&format!("{prefix}{query}"))
            .map_err(|e| budget.sql_error(e))?;
        let mut rows = stmt.query([]).map_err(|e| budget.sql_error(e))?;
        let mut instructions = 0;
        let mut constant_tables = 0;
        while let Some(row) = rows.next().map_err(|e| budget.sql_error(e))? {
            budget.guard()?;
            instructions += 1;
            if instructions > if native { 16384 } else { 1024 } {
                return Err(budget.fail(
                    ErrorCode::IndexCorrupt,
                    "compact audit proof plan is too large",
                ));
            }
            let ValueRef::Text(value) = row.get_ref(column).map_err(|e| budget.sql_error(e))?
            else {
                return Err(
                    budget.fail(ErrorCode::IndexCorrupt, "compact audit invalid proof plan")
                );
            };
            // Exact schema was already validated. Native integrity's only
            // ephemeral table is CHECK(proof_layout_version IN (0,1,2)):
            // three literal constants, independent of table cardinalities.
            // The fixed pragma generator uses direct B-tree scans/lookups.
            // No ephemeral table is permitted in the FTS comparison queries.
            if value == b"OpenEphemeral" {
                constant_tables += 1;
                if !native || constant_tables > 1 {
                    return Err(budget.fail(
                        ErrorCode::IndexCorrupt,
                        "compact audit proof requires an unadmitted temp table",
                    ));
                }
            }
            if value.windows(4).any(|s| s == b"TEMP")
                || matches!(
                    value,
                    b"SorterOpen" | b"SorterInsert" | b"SorterSort" | b"Sort"
                )
            {
                return Err(budget.fail(
                    ErrorCode::IndexCorrupt,
                    "compact audit proof requires a sorter or temp table",
                ));
            }
        }
    }
    Ok(())
}

#[derive(PartialEq, Eq)]
struct Posting<'a> {
    term: &'a [u8],
    doc: i64,
    column: usize,
    offset: i64,
}
struct PriorPosting {
    term: Vec<u8>,
    doc: i64,
    column: usize,
    offset: i64,
}
fn posting<'a>(row: &'a Row<'_>, columns: &[&str], budget: &CheckBudget) -> Result<Posting<'a>> {
    let ValueRef::Text(term) = row.get_ref(0).map_err(|e| budget.sql_error(e))? else {
        return Err(budget.fail(
            ErrorCode::IndexCorrupt,
            "compact audit invalid vocabulary term",
        ));
    };
    let ValueRef::Text(column) = row.get_ref(2).map_err(|e| budget.sql_error(e))? else {
        return Err(budget.fail(
            ErrorCode::IndexCorrupt,
            "compact audit invalid vocabulary column",
        ));
    };
    budget.admit_posting(
        (term.len() as u64)
            .saturating_add(column.len() as u64)
            .saturating_add(24),
    )?;
    let ordinal = columns
        .iter()
        .position(|name| name.as_bytes() == column)
        .ok_or_else(|| {
            budget.fail(
                ErrorCode::IndexCorrupt,
                "compact audit unexpected indexed column",
            )
        })?;
    let doc = row.get(1).map_err(|e| budget.sql_error(e))?;
    let offset: i64 = row.get(3).map_err(|e| budget.sql_error(e))?;
    if offset < 0 {
        return Err(budget.fail(
            ErrorCode::IndexCorrupt,
            "compact audit negative token offset",
        ));
    }
    Ok(Posting {
        term,
        doc,
        column: ordinal,
        offset,
    })
}
fn advance(
    prior: &mut Option<PriorPosting>,
    value: &Posting<'_>,
    budget: &CheckBudget,
) -> Result<()> {
    if prior.as_ref().is_some_and(|p| {
        (p.term.as_slice(), p.doc, p.column, p.offset)
            >= (value.term, value.doc, value.column, value.offset)
    }) {
        return Err(budget.fail(
            ErrorCode::IndexCorrupt,
            "compact audit native token order is not strictly increasing",
        ));
    }
    *prior = Some(PriorPosting {
        term: value.term.to_vec(),
        doc: value.doc,
        column: value.column,
        offset: value.offset,
    });
    Ok(())
}
fn compare_postings(
    source: &Connection,
    scratch: &Connection,
    table: &str,
    columns: &[&str],
    budget: &CheckBudget,
) -> Result<u64> {
    let query = format!("SELECT term,doc,col,offset FROM {table} ORDER BY term");
    require_stream_plan(source, &query, budget)?;
    require_stream_plan(scratch, &query, budget)?;
    let mut a = source.prepare(&query).map_err(|e| budget.sql_error(e))?;
    let mut b = scratch.prepare(&query).map_err(|e| budget.sql_error(e))?;
    let mut a = a.query([]).map_err(|e| budget.sql_error(e))?;
    let mut b = b.query([]).map_err(|e| budget.sql_error(e))?;
    let (mut prior_a, mut prior_b) = (None, None);
    let mut count = 0;
    loop {
        budget.guard()?;
        match (
            a.next().map_err(|e| budget.sql_error(e))?,
            b.next().map_err(|e| budget.sql_error(e))?,
        ) {
            (None, None) => return Ok(count),
            (Some(a), Some(b)) => {
                let a = posting(a, columns, budget)?;
                let b = posting(b, columns, budget)?;
                advance(&mut prior_a, &a, budget)?;
                advance(&mut prior_b, &b, budget)?;
                if a != b {
                    return Err(budget.fail(
                        ErrorCode::IndexCorrupt,
                        "compact audit token positions differ",
                    ));
                }
                count += 1;
            }
            _ => {
                return Err(budget.fail(
                    ErrorCode::IndexCorrupt,
                    "compact audit token stream membership differs",
                ));
            }
        }
    }
}
fn size_row<'a>(row: &'a Row<'_>, budget: &CheckBudget) -> Result<(i64, &'a [u8])> {
    let id = row.get(0).map_err(|e| budget.sql_error(e))?;
    let ValueRef::Blob(blob) = row.get_ref(1).map_err(|e| budget.sql_error(e))? else {
        return Err(budget.fail(
            ErrorCode::IndexCorrupt,
            "compact audit invalid docsize blob",
        ));
    };
    budget.admit_posting((blob.len() as u64).saturating_add(8))?;
    Ok((id, blob))
}
fn compare_docsize(
    source: &Connection,
    scratch: &Connection,
    table: &str,
    budget: &CheckBudget,
) -> Result<u64> {
    let query = format!("SELECT id,sz FROM {table} ORDER BY id");
    require_stream_plan(source, &query, budget)?;
    require_stream_plan(scratch, &query, budget)?;
    let mut a = source.prepare(&query).map_err(|e| budget.sql_error(e))?;
    let mut b = scratch.prepare(&query).map_err(|e| budget.sql_error(e))?;
    let mut a = a.query([]).map_err(|e| budget.sql_error(e))?;
    let mut b = b.query([]).map_err(|e| budget.sql_error(e))?;
    let mut count = 0;
    let mut prior = None;
    loop {
        budget.guard()?;
        match (
            a.next().map_err(|e| budget.sql_error(e))?,
            b.next().map_err(|e| budget.sql_error(e))?,
        ) {
            (None, None) => return Ok(count),
            (Some(a), Some(b)) => {
                let a = size_row(a, budget)?;
                let b = size_row(b, budget)?;
                if a != b || prior.is_some_and(|p| p >= a.0) {
                    return Err(budget.fail(
                        ErrorCode::IndexCorrupt,
                        "compact audit docsize membership or vectors differ",
                    ));
                }
                prior = Some(a.0);
                count += 1;
            }
            _ => {
                return Err(budget.fail(
                    ErrorCode::IndexCorrupt,
                    "compact audit docsize membership differs",
                ));
            }
        }
    }
}
fn compare_totals(
    source: &Connection,
    scratch: &Connection,
    table: &str,
    columns: usize,
    empty: bool,
    budget: &CheckBudget,
) -> Result<()> {
    let query = format!("SELECT block FROM {table} WHERE id=1");
    require_stream_plan(source, &query, budget)?;
    require_stream_plan(scratch, &query, budget)?;
    let mut a = source.prepare(&query).map_err(|e| budget.sql_error(e))?;
    let mut b = scratch.prepare(&query).map_err(|e| budget.sql_error(e))?;
    let mut a = a.query([]).map_err(|e| budget.sql_error(e))?;
    let mut b = b.query([]).map_err(|e| budget.sql_error(e))?;
    match (
        a.next().map_err(|e| budget.sql_error(e))?,
        b.next().map_err(|e| budget.sql_error(e))?,
    ) {
        (Some(a), Some(b)) => {
            let ValueRef::Blob(a) = a.get_ref(0).map_err(|e| budget.sql_error(e))? else {
                return Err(budget.fail(
                    ErrorCode::IndexCorrupt,
                    "compact audit invalid total counters",
                ));
            };
            let ValueRef::Blob(b) = b.get_ref(0).map_err(|e| budget.sql_error(e))? else {
                return Err(budget.fail(
                    ErrorCode::IndexCorrupt,
                    "compact audit invalid reference counters",
                ));
            };
            budget.admit_posting(a.len() as u64)?;
            budget.admit_posting(b.len() as u64)?;
            // SQLite initializes an empty index with X'', but saving totals
            // after deletion of its last row emits nCol+1 zero varints. Both
            // are native empty encodings, not a missing/truncated-row waiver.
            let zeros = [0u8; 9];
            let empty_encoding = |v: &[u8]| v.is_empty() || v == &zeros[..columns + 1];
            if a == b || (empty && empty_encoding(a) && empty_encoding(b)) {
                budget.guard()
            } else {
                Err(budget.fail(
                    ErrorCode::IndexCorrupt,
                    "compact audit total counters differ",
                ))
            }
        }
        _ => Err(budget.fail(
            ErrorCode::IndexCorrupt,
            "compact audit total counter membership differs",
        )),
    }
}

#[cfg(test)]
#[path = "compact_audit_tests.rs"]
mod tests;

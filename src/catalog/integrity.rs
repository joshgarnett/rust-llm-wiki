//! Bind disposable SQL rows to the pinned canonical projection before verification.
use super::{CatalogProjection, sql};
use crate::domain::{ErrorCode, Result, WikiError};
use rusqlite::{Connection, types::Value};

fn rows(connection: &Connection, query: &str, generation: i64) -> Result<Vec<Vec<Value>>> {
    let mut statement = connection.prepare(query).map_err(sql::sql_error)?;
    let columns = statement.column_count();
    let bound = statement.parameter_count() != 0;
    let mut cursor = if bound {
        statement.query([generation])
    } else {
        statement.query([])
    }
    .map_err(sql::sql_error)?;
    let mut result = Vec::new();
    while let Some(row) = cursor.next().map_err(sql::sql_error)? {
        result.push(
            (0..columns)
                .map(|column| row.get(column))
                .collect::<std::result::Result<Vec<Value>, _>>()
                .map_err(sql::sql_error)?,
        );
    }
    Ok(result)
}

/// Regenerate in memory only: cache JSON is never authority for verified SQL rows.
pub(crate) fn validate_projection(
    connection: &Connection,
    generation: i64,
    projection: &CatalogProjection,
) -> Result<()> {
    let mut expected = Connection::open_in_memory().map_err(sql::sql_error)?;
    sql::configure(&expected, 1_000, true)?;
    sql::initialize(&mut expected)?;
    let tx = expected.transaction().map_err(sql::sql_error)?;
    sql::insert_projection(&tx, generation, projection)?;
    sql::replace_documents_fts(&tx, generation, projection)?;
    sql::replace_graph_fts(&tx, generation, projection)?;
    tx.commit().map_err(sql::sql_error)?;
    // Document row numbers are disposable; compare their natural path instead.
    let mut queries = vec![
        ("documents", "SELECT gen,path,record_id,kind,file_hash,title,body,raw_text,source_id,owner_revision,eligibility,row_json FROM documents WHERE gen=?1 ORDER BY path".to_owned()),
        ("documents_fts", "SELECT f.title,f.aliases,f.headings,f.tags,f.body,f.gen,d.path FROM documents_fts f LEFT JOIN documents d ON d.doc_row=f.doc_row AND d.gen=f.gen ORDER BY d.path,f.gen".to_owned()),
        ("graph_fts", "SELECT * FROM graph_fts ORDER BY target_id,target_kind,gen".to_owned()),
    ];
    for (table, order) in [
        ("records", "id"),
        ("aliases", "id,alias"),
        ("entities", "id"),
        ("assertions", "id"),
        ("evidence", "id"),
        ("sources", "id"),
        ("revisions", "id"),
        ("decisions", "id"),
        ("dependencies", "owner_id,path,role"),
        (
            "links",
            "from_path,byte_start,target_id,target_path,resolution",
        ),
        ("diagnostics", "path,record_id,code,details_json"),
    ] {
        queries.push((
            table,
            format!("SELECT * FROM {table} WHERE gen=?1 ORDER BY {order}"),
        ));
    }
    for (table, query) in queries {
        if rows(connection, &query, generation)? != rows(&expected, &query, generation)? {
            return Err(WikiError::new(
                ErrorCode::IndexCorrupt,
                format!("published {table} rows differ from canonical projection; rebuild cache"),
            ));
        }
    }
    Ok(())
}

//! SQLite is a disposable projection; canonical records remain in the vault.
use super::types::CatalogProjection;
use crate::domain::{ErrorCode, Result, WikiError};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};
use std::{path::Path, time::Duration};

pub(crate) const SQL_VERSION: i64 = 1;
pub(crate) fn sql_error(error: rusqlite::Error) -> WikiError {
    WikiError::new(ErrorCode::IndexCorrupt, format!("SQLite catalog: {error}"))
}
pub(crate) fn json<T: serde::Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))
}
pub(crate) fn integer(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| {
        WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "value exceeds SQLite signed integer range",
        )
    })
}
pub(crate) fn configure(connection: &Connection, timeout_ms: u64, writable: bool) -> Result<()> {
    if timeout_ms > 30_000 {
        return Err(WikiError::new(
            ErrorCode::ConfigInvalid,
            "catalog busy timeout exceeds 30 seconds",
        ));
    }
    connection
        .busy_timeout(Duration::from_millis(timeout_ms))
        .map_err(sql_error)?;
    connection
        .pragma_update(None, "foreign_keys", true)
        .map_err(sql_error)?;
    if writable {
        let mode: String = connection
            .query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
            .map_err(sql_error)?;
        if mode != "wal" && mode != "memory" {
            return Err(WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "SQLite WAL unavailable",
            ));
        }
        connection
            .pragma_update(None, "synchronous", "FULL")
            .map_err(sql_error)?;
    }
    Ok(())
}
pub(crate) fn open(path: &Path, timeout_ms: u64, writable: bool) -> Result<Connection> {
    let flags = if writable {
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE
    } else {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    };
    let c = Connection::open_with_flags(path, flags | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(sql_error)?;
    configure(&c, timeout_ms, writable)?;
    Ok(c)
}
pub(crate) fn capability() -> Result<()> {
    let c = Connection::open_in_memory().map_err(sql_error)?;
    c.execute_batch("CREATE VIRTUAL TABLE capability_fts USING fts5(value, tokenize='unicode61 remove_diacritics 2'); INSERT INTO capability_fts VALUES ('café');").map_err(|e|WikiError::new(ErrorCode::CapabilityUnavailable,format!("bundled FTS5 unavailable: {e}")))?;
    let count: i64 = c
        .query_row(
            "SELECT count(*) FROM capability_fts WHERE capability_fts MATCH 'cafe'",
            [],
            |r| r.get(0),
        )
        .map_err(sql_error)?;
    let threaded: i64 = c
        .query_row(
            "SELECT sqlite_compileoption_used('THREADSAFE=1')",
            [],
            |r| r.get(0),
        )
        .map_err(sql_error)?;
    if count != 1 || threaded != 1 {
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "bundled SQLite tokenizer/thread support unavailable",
        ));
    }
    Ok(())
}
pub(crate) fn version(c: &Connection) -> Result<i64> {
    c.pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(sql_error)
}
pub(crate) fn validate(c: &Connection) -> Result<()> {
    let quick: String = c
        .query_row("PRAGMA quick_check", [], |r| r.get(0))
        .map_err(sql_error)?;
    if quick != "ok" {
        return Err(WikiError::new(ErrorCode::IndexCorrupt, quick));
    }
    if version(c)? != SQL_VERSION {
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "cache SQL version requires explicit rebuild",
        ));
    }
    // Prepare expected shapes as well as checking file pages.
    c.prepare("SELECT g.gen,g.parser_hash,g.manifest_hash,g.projection_json FROM generations g JOIN index_meta m ON g.gen=m.published_gen WHERE g.state='complete'").map_err(sql_error)?;
    c.prepare("SELECT gen,doc_row FROM documents_fts WHERE documents_fts MATCH ?1")
        .map_err(sql_error)?;
    c.prepare("SELECT gen,target_id FROM graph_fts WHERE graph_fts MATCH ?1")
        .map_err(sql_error)?;
    for shape in [
        "index_meta(singleton,published_gen,vector_cache_lost,vector_loss_unknown)",
        "documents(doc_row,gen,path,record_id,kind,file_hash,title,body,raw_text,source_id,owner_revision,eligibility,row_json)",
        "records(gen,id,kind,path,hash,authored_status,eligibility,disputed,row_json)",
        "aliases(gen,id,alias)",
        "entities(gen,id,entity_type,identity_eligibility,description_eligibility)",
        "assertions(gen,id,subject_id,predicate,object_id,literal_type,literal_value,qualifiers_json,status)",
        "evidence(gen,id,assertion_id,source_id,revision_id,stance,span_start,span_end,quote_hash,eligibility)",
        "sources(gen,id,current_revision,status,manifest_json)",
        "revisions(gen,id,source_id,original_path,original_hash,content_path,content_hash,extraction_status,manifest_json)",
        "decisions(gen,id,action,status,inputs_json,outputs_json,decision_json)",
        "dependencies(gen,owner_id,path,expected_hash,role)",
        "links(gen,from_path,byte_start,target_id,target_path,resolution)",
        "diagnostics(gen,path,record_id,code,details_json)",
    ] {
        let (table, columns) = shape.split_once('(').expect("constant SQL shape");
        c.prepare(&format!(
            "SELECT {} FROM {table} LIMIT 0",
            columns.trim_end_matches(')')
        ))
        .map_err(sql_error)?;
    }
    let violation: Option<String> = c
        .query_row("PRAGMA foreign_key_check", [], |r| r.get(0))
        .optional()
        .map_err(sql_error)?;
    if violation.is_some() {
        return Err(WikiError::new(
            ErrorCode::IndexCorrupt,
            "catalog foreign key violation",
        ));
    }
    Ok(())
}
const SCHEMA: &str = r#"
CREATE TABLE generations(gen INTEGER PRIMARY KEY CHECK(gen>0),state TEXT NOT NULL CHECK(state IN ('building','complete')),manifest_hash TEXT NOT NULL,parser_hash TEXT NOT NULL,projection_json TEXT NOT NULL);
CREATE TABLE index_meta(singleton INTEGER PRIMARY KEY CHECK(singleton=1),published_gen INTEGER REFERENCES generations(gen),vector_cache_lost INTEGER NOT NULL CHECK(vector_cache_lost IN (0,1)),vector_loss_unknown INTEGER NOT NULL CHECK(vector_loss_unknown IN (0,1)));
INSERT INTO index_meta VALUES(1,NULL,0,1);
CREATE TABLE documents(doc_row INTEGER PRIMARY KEY,gen INTEGER NOT NULL REFERENCES generations(gen),path TEXT NOT NULL,record_id TEXT,kind TEXT,file_hash TEXT NOT NULL,title TEXT NOT NULL,body TEXT NOT NULL,raw_text TEXT NOT NULL,source_id TEXT,owner_revision TEXT,eligibility TEXT NOT NULL,row_json TEXT NOT NULL,UNIQUE(gen,path));
CREATE UNIQUE INDEX record_ids ON documents(gen,record_id) WHERE record_id IS NOT NULL;
CREATE TABLE records(gen INTEGER NOT NULL REFERENCES generations(gen),id TEXT NOT NULL,kind TEXT NOT NULL,path TEXT NOT NULL,hash TEXT NOT NULL,authored_status TEXT,eligibility TEXT NOT NULL,disputed INTEGER NOT NULL,row_json TEXT NOT NULL,PRIMARY KEY(gen,id));
CREATE TABLE aliases(gen INTEGER NOT NULL,id TEXT NOT NULL,alias TEXT NOT NULL,PRIMARY KEY(gen,id,alias),FOREIGN KEY(gen,id) REFERENCES records(gen,id));
CREATE TABLE entities(gen INTEGER NOT NULL,id TEXT NOT NULL,entity_type TEXT,identity_eligibility TEXT,description_eligibility TEXT,PRIMARY KEY(gen,id),FOREIGN KEY(gen,id) REFERENCES records(gen,id));
CREATE TABLE assertions(gen INTEGER NOT NULL,id TEXT NOT NULL,subject_id TEXT,predicate TEXT,object_id TEXT,literal_type TEXT,literal_value TEXT,qualifiers_json TEXT NOT NULL,status TEXT,PRIMARY KEY(gen,id),FOREIGN KEY(gen,id) REFERENCES records(gen,id));
CREATE INDEX outgoing ON assertions(gen,subject_id,predicate);
CREATE INDEX incoming ON assertions(gen,object_id,predicate);
CREATE TABLE evidence(gen INTEGER NOT NULL,id TEXT NOT NULL,assertion_id TEXT,source_id TEXT,revision_id TEXT,stance TEXT,span_start INTEGER,span_end INTEGER,quote_hash TEXT,eligibility TEXT NOT NULL,PRIMARY KEY(gen,id),FOREIGN KEY(gen,id) REFERENCES records(gen,id));
CREATE TABLE sources(gen INTEGER NOT NULL,id TEXT NOT NULL,current_revision TEXT,status TEXT,manifest_json TEXT NOT NULL,PRIMARY KEY(gen,id),FOREIGN KEY(gen,id) REFERENCES records(gen,id));
CREATE TABLE revisions(gen INTEGER NOT NULL,id TEXT NOT NULL,source_id TEXT,original_path TEXT,original_hash TEXT,content_path TEXT,content_hash TEXT,extraction_status TEXT,manifest_json TEXT NOT NULL,PRIMARY KEY(gen,id),FOREIGN KEY(gen,id) REFERENCES records(gen,id));
CREATE TABLE decisions(gen INTEGER NOT NULL,id TEXT NOT NULL,action TEXT,status TEXT,inputs_json TEXT,outputs_json TEXT,decision_json TEXT NOT NULL,PRIMARY KEY(gen,id),FOREIGN KEY(gen,id) REFERENCES records(gen,id));
CREATE TABLE dependencies(gen INTEGER NOT NULL REFERENCES generations(gen),owner_id TEXT NOT NULL,path TEXT NOT NULL,expected_hash TEXT,role TEXT NOT NULL,PRIMARY KEY(gen,owner_id,path,role));
CREATE TABLE links(gen INTEGER NOT NULL REFERENCES generations(gen),from_path TEXT NOT NULL,byte_start INTEGER NOT NULL,target_id TEXT,target_path TEXT,resolution TEXT NOT NULL);
CREATE TABLE diagnostics(gen INTEGER NOT NULL REFERENCES generations(gen),path TEXT NOT NULL,record_id TEXT,code TEXT NOT NULL,details_json TEXT NOT NULL);
CREATE VIRTUAL TABLE documents_fts USING fts5(title,aliases,headings,tags,body,gen UNINDEXED,doc_row UNINDEXED,tokenize='unicode61 remove_diacritics 2');
CREATE VIRTUAL TABLE graph_fts USING fts5(name,aliases,endpoints,predicate,qualifiers,description,target_kind UNINDEXED,target_id UNINDEXED,gen UNINDEXED,tokenize='unicode61 remove_diacritics 2');
PRAGMA user_version=1;
"#;
pub(crate) fn initialize(c: &mut Connection) -> Result<()> {
    let tx = c.transaction().map_err(sql_error)?;
    tx.execute_batch(SCHEMA).map_err(sql_error)?;
    tx.commit().map_err(sql_error)
}
/// Recreate tables in the existing file, never unlink/replace an open database.
pub(crate) fn reset(c: &Transaction<'_>) -> Result<bool> {
    let tables: Vec<String> = {
        let mut s = c
            .prepare(
                "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
            )
            .map_err(sql_error)?;
        s.query_map([], |r| r.get(0))
            .map_err(sql_error)?
            .collect::<std::result::Result<_, _>>()
            .map_err(sql_error)?
    };
    let mut vector_loss = cache_loss_notices(c).map(|v| v.0).unwrap_or(false);
    for table in tables
        .iter()
        .filter(|t| t.contains("vector") || t.contains("embedding"))
    {
        let escaped = table.replace('"', "\"\"");
        let exists: i64 = c
            .query_row(
                &format!("SELECT EXISTS(SELECT 1 FROM \"{escaped}\" LIMIT 1)"),
                [],
                |r| r.get(0),
            )
            .map_err(sql_error)?;
        vector_loss |= exists != 0;
    }
    {
        let tx = c;
        // Virtual parents drop their internal tables automatically.
        for name in ["documents_fts", "graph_fts"] {
            if tables.iter().any(|t| t == name) {
                tx.execute_batch(&format!("DROP TABLE \"{name}\";"))
                    .map_err(sql_error)?;
            }
        }
        for name in &tables {
            if name == "documents_fts"
                || name == "graph_fts"
                || name.starts_with("documents_fts_")
                || name.starts_with("graph_fts_")
            {
                continue;
            }
            let escaped = name.replace('"', "\"\"");
            tx.execute_batch(&format!("DROP TABLE IF EXISTS \"{escaped}\";"))
                .map_err(sql_error)?;
        }
        tx.execute_batch(SCHEMA).map_err(sql_error)?;
    }
    Ok(vector_loss)
}
pub(crate) fn cache_loss_notices(c: &Connection) -> Result<(bool, bool)> {
    c.query_row(
        "SELECT vector_cache_lost,vector_loss_unknown FROM index_meta WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .map_err(sql_error)
}
pub(crate) fn insert_projection(
    tx: &Transaction<'_>,
    generation: i64,
    p: &CatalogProjection,
) -> Result<()> {
    tx.execute(
        "INSERT INTO generations VALUES(?1,'building',?2,?3,?4)",
        params![
            generation,
            p.control_manifest.as_str(),
            p.parser_fingerprint.as_str(),
            json(p)?
        ],
    )
    .map_err(sql_error)?;
    for d in &p.documents {
        tx.execute("INSERT INTO documents(gen,path,record_id,kind,file_hash,title,body,raw_text,source_id,owner_revision,eligibility,row_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",params![generation,d.path.as_str(),d.record_id.as_ref().map(|v|v.as_str()),d.kind.map(|v|v.to_string()),d.hash.as_str(),d.title,d.body,d.raw_text,d.source_id.as_ref().map(|v|v.as_str()),d.owner_revision.as_ref().map(|v|v.as_str()),json(&d.eligibility)?.trim_matches('"'),json(d)?]).map_err(sql_error)?;
    }
    for (id, row) in &p.records {
        let r = &row.record;
        let s = |key| r.string(key);
        let eligibility = json(&row.eligibility)?;
        let status = row.authored_status.as_deref();
        tx.execute(
            "INSERT INTO records VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                generation,
                id.as_str(),
                r.kind().to_string(),
                row.path.as_str(),
                row.hash.as_str(),
                status,
                eligibility.trim_matches('"'),
                row.disputed,
                json(row)?
            ],
        )
        .map_err(sql_error)?;
        if let Some(aliases) = r.field("aliases").and_then(|v| v.as_array()) {
            for alias in aliases {
                tx.execute(
                    "INSERT OR IGNORE INTO aliases VALUES(?1,?2,?3)",
                    params![generation, id.as_str(), alias.as_str()],
                )
                .map_err(sql_error)?;
            }
        }
        use crate::domain::RecordKind;
        match r.kind() {
            RecordKind::Entity => {
                tx.execute(
                    "INSERT INTO entities VALUES(?1,?2,?3,?4,?5)",
                    params![
                        generation,
                        id.as_str(),
                        s("wiki_entity_type"),
                        row.identity_eligibility
                            .map(|v| json(&v))
                            .transpose()?
                            .map(|v| v.trim_matches('"').to_owned()),
                        row.description_eligibility
                            .map(|v| json(&v))
                            .transpose()?
                            .map(|v| v.trim_matches('"').to_owned())
                    ],
                )
                .map_err(sql_error)?;
            }
            RecordKind::Assertion => {
                tx.execute(
                    "INSERT INTO assertions VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                    params![
                        generation,
                        id.as_str(),
                        s("wiki_subject_id"),
                        s("wiki_predicate"),
                        s("wiki_object_id"),
                        s("wiki_literal_type"),
                        s("wiki_literal_value"),
                        json(r.fields())?,
                        status
                    ],
                )
                .map_err(sql_error)?;
            }
            RecordKind::Evidence => {
                let number = |key| {
                    r.field(key)
                        .and_then(|v| v.as_u64())
                        .map(|value| match integer(value) {
                            Err(_) if row.eligibility == crate::domain::Eligibility::Invalid => {
                                Ok(None)
                            }
                            result => result.map(Some),
                        })
                        .transpose()
                        .map(Option::flatten)
                };
                tx.execute(
                    "INSERT INTO evidence VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                    params![
                        generation,
                        id.as_str(),
                        s("wiki_assertion_id"),
                        s("wiki_source_id"),
                        s("wiki_source_revision"),
                        s("wiki_stance"),
                        number("wiki_span_start")?,
                        number("wiki_span_end")?,
                        s("wiki_quote_hash"),
                        eligibility.trim_matches('"')
                    ],
                )
                .map_err(sql_error)?;
            }
            RecordKind::Source => {
                tx.execute(
                    "INSERT INTO sources VALUES(?1,?2,?3,?4,?5)",
                    params![
                        generation,
                        id.as_str(),
                        s("wiki_current_revision"),
                        status,
                        json(r)?
                    ],
                )
                .map_err(sql_error)?;
            }
            RecordKind::Revision => {
                tx.execute(
                    "INSERT INTO revisions VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                    params![
                        generation,
                        id.as_str(),
                        s("wiki_source_id"),
                        s("wiki_original_path"),
                        s("wiki_original_hash"),
                        s("wiki_content_path"),
                        s("wiki_content_hash"),
                        s("wiki_extraction_status"),
                        json(r)?
                    ],
                )
                .map_err(sql_error)?;
            }
            RecordKind::Decision => {
                tx.execute(
                    "INSERT INTO decisions VALUES(?1,?2,?3,?4,?5,?6,?7)",
                    params![
                        generation,
                        id.as_str(),
                        s("wiki_action"),
                        status,
                        json(&r.field("wiki_input_ids"))?,
                        json(&r.field("wiki_output_ids"))?,
                        json(r)?
                    ],
                )
                .map_err(sql_error)?;
            }
            _ => {}
        }
        for dep in &row.dependencies {
            insert_dependency(tx, generation, id.as_str(), dep, "record")?;
        }
    }
    for dep in &p.dependencies {
        insert_dependency(tx, generation, "", dep, "manifest")?;
    }
    for link in &p.links {
        tx.execute(
            "INSERT INTO links VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                generation,
                link.from_path.as_str(),
                integer(link.byte_start)?,
                link.target_id.as_ref().map(|v| v.as_str()),
                link.target_path.as_ref().map(|v| v.as_str()),
                link.resolution
            ],
        )
        .map_err(sql_error)?;
    }
    for d in &p.diagnostics {
        tx.execute(
            "INSERT INTO diagnostics VALUES(?1,?2,?3,?4,?5)",
            params![
                generation,
                d.path.as_str(),
                d.record_id.as_ref().map(|v| v.as_str()),
                d.code.to_string(),
                json(&d.details)?
            ],
        )
        .map_err(sql_error)?;
    }
    Ok(())
}
fn insert_dependency(
    tx: &Transaction<'_>,
    generation: i64,
    owner: &str,
    d: &crate::changes::ReadDependency,
    role: &str,
) -> Result<()> {
    let hash = match &d.expected {
        crate::vault::ExpectedState::Absent => None,
        crate::vault::ExpectedState::Hash(h) => Some(h.as_str()),
    };
    tx.execute(
        "INSERT INTO dependencies VALUES(?1,?2,?3,?4,?5)",
        params![generation, owner, d.path.as_str(), hash, role],
    )
    .map_err(sql_error)?;
    Ok(())
}
pub(crate) fn replace_documents_fts(
    tx: &Transaction<'_>,
    generation: i64,
    p: &CatalogProjection,
) -> Result<()> {
    tx.execute("DELETE FROM documents_fts", [])
        .map_err(sql_error)?;
    for d in &p.documents {
        let row: i64 = tx
            .query_row(
                "SELECT doc_row FROM documents WHERE gen=?1 AND path=?2",
                params![generation, d.path.as_str()],
                |r| r.get(0),
            )
            .map_err(sql_error)?;
        tx.execute(
            "INSERT INTO documents_fts VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                d.title,
                d.aliases.join(" "),
                d.headings,
                d.tags.join(" "),
                d.body,
                generation,
                row
            ],
        )
        .map_err(sql_error)?;
    }
    Ok(())
}
pub(crate) fn replace_graph_fts(
    tx: &Transaction<'_>,
    generation: i64,
    p: &CatalogProjection,
) -> Result<()> {
    tx.execute("DELETE FROM graph_fts", []).map_err(sql_error)?;
    for g in &p.graph {
        tx.execute(
            "INSERT INTO graph_fts VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                g.name,
                g.aliases.join(" "),
                g.endpoints,
                g.predicate,
                g.qualifiers,
                g.description,
                g.target_kind.to_string(),
                g.target_id.as_str(),
                generation
            ],
        )
        .map_err(sql_error)?;
    }
    Ok(())
}

/// WAL readers retain old pages even after the new writer reclaims ordinary rows.
pub(crate) fn prune_generations(tx: &Transaction<'_>, published: i64) -> Result<()> {
    for table in [
        "aliases",
        "entities",
        "assertions",
        "evidence",
        "sources",
        "revisions",
        "decisions",
        "dependencies",
        "links",
        "diagnostics",
        "records",
        "documents",
        "generations",
    ] {
        tx.execute(&format!("DELETE FROM {table} WHERE gen<>?1"), [published])
            .map_err(sql_error)?;
    }
    Ok(())
}

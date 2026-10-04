//! One current normalized projection per catalog sibling, with mutable epochs.
//! This schema is activated only after all serving/publication paths can use it.
pub(crate) const SCHEMA: &str = r#"
CREATE TABLE catalog_meta(
 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
 schema_version INTEGER NOT NULL CHECK(schema_version=3),
 vault_id TEXT NOT NULL,file_id TEXT NOT NULL,creation_epoch INTEGER NOT NULL CHECK(creation_epoch>0),
 creation_header_hash TEXT NOT NULL,epoch INTEGER NOT NULL CHECK(epoch>=creation_epoch),
 parser_hash TEXT,publication_hash TEXT,control_hash TEXT,dependency_hash TEXT,audit_epoch INTEGER,
 state TEXT NOT NULL CHECK(state IN ('building','complete')),
 origin_change_id TEXT,origin_manifest_hash TEXT,
 vector_cache_lost INTEGER NOT NULL CHECK(vector_cache_lost IN (0,1)),
 vector_loss_unknown INTEGER NOT NULL CHECK(vector_loss_unknown IN (0,1)),
 CHECK((origin_change_id IS NULL)=(origin_manifest_hash IS NULL)),
 CHECK(state='building' OR (parser_hash IS NOT NULL AND publication_hash IS NOT NULL)),
 CHECK((audit_epoch IS NULL AND control_hash IS NULL AND dependency_hash IS NULL) OR
       (audit_epoch IS NOT NULL AND control_hash IS NOT NULL AND dependency_hash IS NOT NULL AND audit_epoch>0 AND audit_epoch<=epoch))
);
CREATE TABLE documents(
 doc_row INTEGER PRIMARY KEY,path TEXT COLLATE BINARY NOT NULL UNIQUE,
 record_id TEXT,kind TEXT,file_hash TEXT NOT NULL,title TEXT NOT NULL,
 aliases_json TEXT NOT NULL,aliases_text TEXT NOT NULL,headings TEXT NOT NULL,
 tags_json TEXT NOT NULL,tags_text TEXT NOT NULL,body TEXT NOT NULL,raw_text TEXT NOT NULL,
 source_id TEXT,owner_revision TEXT,eligibility TEXT NOT NULL,reasons_json TEXT NOT NULL
);
CREATE UNIQUE INDEX document_record_ids ON documents(record_id) WHERE record_id IS NOT NULL;
CREATE INDEX source_document_ids ON documents(source_id,path) WHERE owner_revision IS NOT NULL AND source_id IS NOT NULL AND eligibility='current';
CREATE INDEX source_revision_ids ON documents(owner_revision,path) WHERE owner_revision IS NOT NULL AND source_id IS NOT NULL AND eligibility='current';
CREATE INDEX source_document_titles ON documents(title,path) WHERE owner_revision IS NOT NULL AND source_id IS NOT NULL AND eligibility='current';
CREATE VIEW document_fts_content AS SELECT doc_row,title,aliases_text AS aliases,headings,tags_text AS tags,body FROM documents;
CREATE VIRTUAL TABLE documents_fts USING fts5(title,aliases,headings,tags,body,content='document_fts_content',content_rowid='doc_row',detail=full,columnsize=1,tokenize='unicode61 remove_diacritics 2');
CREATE VIRTUAL TABLE documents_vocab USING fts5vocab(documents_fts,'instance');
CREATE TABLE graph_rows(
 graph_row INTEGER PRIMARY KEY,target_id TEXT NOT NULL UNIQUE,target_kind TEXT NOT NULL,
 name TEXT NOT NULL,aliases_json TEXT NOT NULL,aliases_text TEXT NOT NULL,endpoints TEXT NOT NULL,
 predicate TEXT NOT NULL,qualifiers TEXT NOT NULL,description TEXT NOT NULL
);
CREATE VIEW graph_fts_content AS SELECT graph_row,name,aliases_text AS aliases,endpoints,predicate,qualifiers,description,target_kind,target_id FROM graph_rows;
CREATE VIRTUAL TABLE graph_fts USING fts5(name,aliases,endpoints,predicate,qualifiers,description,target_kind UNINDEXED,target_id UNINDEXED,content='graph_fts_content',content_rowid='graph_row',detail=full,columnsize=1,tokenize='unicode61 remove_diacritics 2');
CREATE VIRTUAL TABLE graph_vocab USING fts5vocab(graph_fts,'instance');
CREATE TABLE records(id TEXT PRIMARY KEY,kind TEXT NOT NULL,path TEXT NOT NULL,hash TEXT NOT NULL,authored_status TEXT,eligibility TEXT NOT NULL,identity_eligibility TEXT,description_eligibility TEXT,disputed INTEGER NOT NULL CHECK(disputed IN (0,1)),row_json TEXT NOT NULL);
CREATE UNIQUE INDEX record_paths ON records(path);
CREATE TABLE identity_claims(record_id TEXT NOT NULL,path TEXT NOT NULL,file_hash TEXT NOT NULL,kind TEXT,PRIMARY KEY(record_id,path));
CREATE TABLE source_revision_identity(source_id TEXT NOT NULL,revision_id TEXT NOT NULL,retained_ordinal INTEGER NOT NULL CHECK(retained_ordinal>=0),original_hash TEXT NOT NULL,content_hash TEXT,extractor_fingerprint TEXT NOT NULL,extraction_status TEXT NOT NULL,PRIMARY KEY(source_id,revision_id),UNIQUE(source_id,retained_ordinal));
CREATE INDEX source_revision_matches ON source_revision_identity(source_id,original_hash,content_hash,extractor_fingerprint,retained_ordinal);
CREATE TABLE source_evidence(source_id TEXT NOT NULL,evidence_id TEXT NOT NULL,assertion_id TEXT NOT NULL,PRIMARY KEY(source_id,evidence_id));
CREATE INDEX source_assertions ON source_evidence(source_id,assertion_id,evidence_id);
CREATE INDEX assertion_evidence ON source_evidence(assertion_id,evidence_id);
CREATE TABLE links(link_row INTEGER PRIMARY KEY,from_path TEXT NOT NULL,byte_start INTEGER NOT NULL CHECK(byte_start>=0),target_id TEXT,target_path TEXT,resolution TEXT NOT NULL);
CREATE INDEX link_paths ON links(from_path,byte_start,link_row);
CREATE TABLE diagnostics(diagnostic_row INTEGER PRIMARY KEY,path TEXT NOT NULL,record_id TEXT,code TEXT NOT NULL,details_json TEXT NOT NULL);
CREATE INDEX diagnostic_paths ON diagnostics(path);
CREATE TABLE dependencies(dependency_row INTEGER PRIMARY KEY,path TEXT NOT NULL UNIQUE,expected_hash TEXT);
PRAGMA user_version=3;
"#;

/// Exact decoder order. Text is never embedded in an additional document JSON.
pub(crate) const DOCUMENT_COLUMNS: &str = "path,file_hash,record_id,kind,title,aliases_json,headings,tags_json,body,raw_text,source_id,owner_revision,eligibility,reasons_json";

//! Bounded decoding of normalized rows, before any owned text allocation.
use super::{
    DocumentRow,
    file_types::{CatalogSelection, ChangeBinding},
    sql,
};
use crate::domain::{
    Blake3Hash, ErrorCode, ReadSnapshot, RecordId, Result, VaultRelativePath, WikiError,
};
use rusqlite::{Connection, Row, types::ValueRef};
use serde::de::DeserializeOwned;

pub(crate) fn corrupt(message: impl Into<String>) -> WikiError {
    WikiError::new(ErrorCode::IndexCorrupt, message)
}

pub(crate) struct CatalogHeader {
    pub snapshot: ReadSnapshot,
    pub audit: Option<AuditObservation>,
    pub origin: Option<ChangeBinding>,
    pub vector_cache_lost: bool,
    pub vector_loss_unknown: bool,
}

/// A full-build/audit observation is historical once its epoch is superseded.
/// Incremental publication may clear this; it must never relabel it as current.
pub(crate) struct AuditObservation {
    pub epoch: u64,
    pub control_hash: Blake3Hash,
    pub dependency_hash: Blake3Hash,
}

/// Read inside the transaction opened while the selector acquisition gate is
/// held. This verifies physical identity, not ordinary-row or posting integrity.
pub(crate) fn header(
    connection: &Connection,
    selection: &CatalogSelection,
) -> Result<CatalogHeader> {
    selection.validate(&selection.vault_id)?;
    if sql::version(connection)? != 3 {
        return Err(corrupt("selected catalog schema version is not 3"));
    }
    let mode: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .map_err(sql::sql_error)?;
    if !mode.eq_ignore_ascii_case("delete") && !mode.eq_ignore_ascii_case("wal") {
        return Err(corrupt(
            "selected catalog requires DELETE or WAL journal mode",
        ));
    }
    let mut statement = connection.prepare(
        "SELECT schema_version,vault_id,file_id,creation_epoch,creation_header_hash,epoch,parser_hash,publication_hash,state,origin_change_id,origin_manifest_hash,vector_cache_lost,vector_loss_unknown,audit_epoch,control_hash,dependency_hash FROM catalog_meta WHERE singleton=1"
    ).map_err(sql::sql_error)?;
    let mut rows = statement.query([]).map_err(sql::sql_error)?;
    let row = rows
        .next()
        .map_err(sql::sql_error)?
        .ok_or_else(|| corrupt("selected catalog header is absent"))?;
    let mut bytes = 0usize;
    for index in 0..16 {
        if let ValueRef::Text(value) = row.get_ref(index).map_err(sql::sql_error)? {
            bytes = bytes
                .checked_add(value.len())
                .ok_or_else(|| corrupt("catalog header byte count overflow"))?;
        }
    }
    if bytes > 16 * 1024 {
        return Err(corrupt("selected catalog header exceeds 16 KiB"));
    }
    let epoch: i64 = row.get(5).map_err(sql::sql_error)?;
    if row.get::<_, i64>(0).map_err(sql::sql_error)? != 3
        || text(row, 1)? != selection.vault_id.as_str()
        || text(row, 2)? != selection.file_id
        || row.get::<_, i64>(3).map_err(sql::sql_error)? != sql::integer(selection.creation_epoch)?
        || text(row, 4)? != selection.creation_header_hash.as_str()
        || epoch < sql::integer(selection.creation_epoch)?
        || text(row, 8)? != "complete"
    {
        return Err(corrupt(
            "selected catalog header does not match its selector",
        ));
    }
    let hash = |index| Blake3Hash::new(text(row, index)?).map_err(|e| corrupt(e.message));
    let origin = match (optional_text(row, 9)?, optional_text(row, 10)?) {
        (None, None) => None,
        (Some(change_id), Some(manifest_hash)) => Some(ChangeBinding {
            change_id: RecordId::new(change_id).map_err(|e| corrupt(e.message))?,
            manifest_hash: Blake3Hash::new(manifest_hash).map_err(|e| corrupt(e.message))?,
        }),
        _ => return Err(corrupt("catalog publication origin is incomplete")),
    };
    let flag = |index| match row.get::<_, i64>(index).map_err(sql::sql_error)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(corrupt("catalog loss notice is not boolean")),
    };
    let audit = match (
        row.get::<_, Option<i64>>(13).map_err(sql::sql_error)?,
        optional_text(row, 14)?,
        optional_text(row, 15)?,
    ) {
        (None, None, None) => None,
        (Some(audit_epoch), Some(control), Some(dependencies))
            if audit_epoch > 0 && audit_epoch <= epoch =>
        {
            Some(AuditObservation {
                epoch: audit_epoch as u64,
                control_hash: Blake3Hash::new(control).map_err(|e| corrupt(e.message))?,
                dependency_hash: Blake3Hash::new(dependencies).map_err(|e| corrupt(e.message))?,
            })
        }
        _ => {
            return Err(corrupt(
                "catalog audit observation is incomplete or newer than publication",
            ));
        }
    };
    let result = CatalogHeader {
        snapshot: ReadSnapshot::published(
            epoch as u64,
            hash(6)?,
            selection.file_id.clone(),
            hash(7)?,
        )
        .map_err(|e| corrupt(e.message))?,
        audit,
        origin,
        vector_cache_lost: flag(11)?,
        vector_loss_unknown: flag(12)?,
    };
    if rows.next().map_err(sql::sql_error)?.is_some() {
        return Err(corrupt("selected catalog header is not unique"));
    }
    Ok(result)
}

fn text<'a>(row: &'a Row<'_>, column: usize) -> Result<&'a str> {
    row.get_ref(column)
        .map_err(sql::sql_error)?
        .as_str()
        .map_err(|e| corrupt(e.to_string()))
}

fn optional_text<'a>(row: &'a Row<'_>, column: usize) -> Result<Option<&'a str>> {
    match row.get_ref(column).map_err(sql::sql_error)? {
        ValueRef::Null => Ok(None),
        ValueRef::Text(bytes) => std::str::from_utf8(bytes)
            .map(Some)
            .map_err(|e| corrupt(e.to_string())),
        _ => Err(corrupt("normalized optional value must be text or null")),
    }
}

fn json<T: DeserializeOwned>(value: &str) -> Result<T> {
    serde_json::from_str(value).map_err(|e| corrupt(e.to_string()))
}

/// `column` starts the exact DOCUMENT_COLUMNS shape. The caller reserves this
/// row's total stored UTF-8 bytes against both per-row and cumulative budgets.
/// Invalid rows remain charged once admitted. No owned column exists before
/// this reservation; SQL's own value/page/work limits are separately required.
pub(crate) fn document(
    row: &Row<'_>,
    column: usize,
    reserve: impl FnOnce(usize) -> Result<()>,
) -> Result<DocumentRow> {
    let mut bytes = 0usize;
    for index in 0..14 {
        let length = match row.get_ref(column + index).map_err(sql::sql_error)? {
            ValueRef::Text(bytes) => bytes.len(),
            ValueRef::Null if matches!(index, 2 | 3 | 10 | 11) => 0,
            _ => return Err(corrupt("normalized document column has the wrong SQL type")),
        };
        bytes = bytes.checked_add(length).ok_or_else(|| {
            WikiError::new(
                ErrorCode::BudgetExceeded,
                "normalized row byte count overflow",
            )
        })?;
    }
    reserve(bytes)?;
    let id = |index| {
        optional_text(row, column + index)?
            .map(|value| RecordId::new(value).map_err(|e| corrupt(e.message)))
            .transpose()
    };
    let kind = optional_text(row, column + 3)?
        .map(|value| value.parse().map_err(|e: WikiError| corrupt(e.message)))
        .transpose()?;
    let eligibility = serde::Deserialize::deserialize(serde::de::value::StrDeserializer::<
        serde::de::value::Error,
    >::new(text(row, column + 12)?))
    .map_err(|e| corrupt(e.to_string()))?;
    Ok(DocumentRow {
        path: VaultRelativePath::new(text(row, column)?).map_err(|e| corrupt(e.message))?,
        hash: Blake3Hash::new(text(row, column + 1)?).map_err(|e| corrupt(e.message))?,
        record_id: id(2)?,
        kind,
        title: text(row, column + 4)?.to_owned(),
        aliases: json(text(row, column + 5)?)?,
        headings: text(row, column + 6)?.to_owned(),
        tags: json(text(row, column + 7)?)?,
        body: text(row, column + 8)?.to_owned(),
        raw_text: text(row, column + 9)?.to_owned(),
        source_id: id(10)?,
        owner_revision: id(11)?,
        eligibility,
        reasons: json(text(row, column + 13)?)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{Connection, params};
    use std::cell::Cell;

    #[test]
    fn published_header_keeps_audit_observations_separate_and_epoch_bound() {
        let temp = tempfile::tempdir().unwrap();
        let connection = Connection::open(temp.path().join("header.sqlite")).unwrap();
        connection
            .execute_batch(super::super::normalized_schema::SCHEMA)
            .unwrap();
        let selected = CatalogSelection::new(RecordId::new("vault_header").unwrap(), 1).unwrap();
        let hash = Blake3Hash::digest("header fixture");
        connection.execute("INSERT INTO catalog_meta(singleton,schema_version,vault_id,file_id,creation_epoch,creation_header_hash,epoch,parser_hash,publication_hash,control_hash,dependency_hash,audit_epoch,state,vector_cache_lost,vector_loss_unknown) VALUES(1,3,?1,?2,1,?3,1,?4,?4,?4,?4,1,'complete',0,0)", params![selected.vault_id.as_str(),selected.file_id,selected.creation_header_hash.as_str(),hash.as_str()]).unwrap();
        let first = header(&connection, &selected).unwrap();
        assert_eq!(
            first.snapshot.publication().unwrap().file_id,
            selected.file_id
        );
        assert!(first.snapshot.require_canonical_manifest().is_err());
        assert_eq!(first.audit.as_ref().unwrap().epoch, 1);
        assert_eq!(first.audit.as_ref().unwrap().control_hash, hash);

        // A retained observation remains explicitly historical after a delta.
        connection
            .execute(
                "UPDATE catalog_meta SET epoch=2,publication_hash=?1",
                [Blake3Hash::digest("delta").as_str()],
            )
            .unwrap();
        let delta = header(&connection, &selected).unwrap();
        assert_eq!(delta.snapshot.generation, 2);
        assert_ne!(delta.snapshot.publication(), first.snapshot.publication());
        assert_eq!(delta.audit.unwrap().epoch, 1);
        connection
            .execute_batch(
                "UPDATE catalog_meta SET audit_epoch=NULL,control_hash=NULL,dependency_hash=NULL",
            )
            .unwrap();
        assert!(header(&connection, &selected).unwrap().audit.is_none());

        // Deliberately bypass SQL constraints to exercise the read boundary.
        connection
            .execute_batch("PRAGMA ignore_check_constraints=ON")
            .unwrap();
        for (epoch, control, dependencies) in [
            (Some(3), Some(hash.as_str()), Some(hash.as_str())),
            (Some(0), Some(hash.as_str()), Some(hash.as_str())),
            (Some(1), None, Some(hash.as_str())),
            (None, Some(hash.as_str()), Some(hash.as_str())),
        ] {
            connection
                .execute(
                    "UPDATE catalog_meta SET audit_epoch=?1,control_hash=?2,dependency_hash=?3",
                    params![epoch, control, dependencies],
                )
                .unwrap();
            assert!(header(&connection, &selected).is_err());
        }
        connection
            .execute_batch("UPDATE catalog_meta SET schema_version=2; PRAGMA user_version=2")
            .unwrap();
        assert!(header(&connection, &selected).is_err());
    }

    #[test]
    fn real_projector_builder_and_selector_keep_old_reader_consistent() {
        use crate::{
            catalog::{
                file_types::BuildIdentity,
                normalized_build::{BuildLimits, NormalizedBuilder},
                scan, selector,
            },
            vault::{VaultFs, VaultRoot, WriterPermit},
        };
        use std::time::Duration;
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("WIKI.md"), "---\nwiki_schema: \"1\"\nwiki_id: vault_integration\nwiki_kind: vault\ntitle: Integration\n---\nWiki\n").unwrap();
        let vault = RecordId::new("vault_integration").unwrap();
        let fs = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(1)).unwrap();
        let build = |epoch, body| {
            std::fs::write(temp.path().join("page.md"), format!("---\nwiki_schema: \"1\"\nwiki_id: page_integration\nwiki_kind: page\ntitle: Page\n---\n{body}\n")).unwrap();
            let identity = BuildIdentity {
                selection: CatalogSelection::new(vault.clone(), epoch).unwrap(),
                origin: None,
                vector_cache_lost: false,
                vector_loss_unknown: false,
            };
            selector::prepare(&fs, &writer, &identity.selection).unwrap();
            let mut builder =
                NormalizedBuilder::begin(&fs, &writer, identity, BuildLimits::default()).unwrap();
            let input = scan::scan_input(&fs, &vault).unwrap();
            let projection = scan::project_with_sink(&fs, &input, false, &mut builder).unwrap();
            let completed = builder.finish(&projection).unwrap();
            assert_eq!(completed.snapshot.generation, epoch);
            selector::publish(
                &fs,
                &writer,
                &completed.identity.selection,
                Duration::from_secs(1),
            )
            .unwrap();
            completed
        };
        let open = || {
            selector::acquire(&fs, &vault, Duration::from_secs(1), |path, selected| {
                let connection = sql::open(path, 1000, false)?;
                connection
                    .execute_batch("BEGIN DEFERRED")
                    .map_err(sql::sql_error)?;
                header(&connection, selected)?;
                Ok(connection)
            })
            .unwrap()
            .unwrap()
        };
        let first = build(1, "oldneedle");
        let held = open();
        let second = build(2, "newneedle");
        let current = open();
        for (reader, token, epoch) in [(&held, "oldneedle", 1), (&current, "newneedle", 2)] {
            let metadata = header(reader.value(), reader.selection()).unwrap();
            assert_eq!(metadata.snapshot.generation, epoch);
            assert!(metadata.origin.is_none());
            assert!(!metadata.vector_cache_lost && !metadata.vector_loss_unknown);
            assert_eq!(
                metadata.audit.as_ref().unwrap().dependency_hash,
                if epoch == 1 {
                    first.dependency_hash.clone()
                } else {
                    second.dependency_hash.clone()
                }
            );
            assert_eq!(
                reader
                    .value()
                    .query_row(
                        "SELECT count(*) FROM documents_fts WHERE documents_fts MATCH ?1",
                        [token],
                        |row| row.get::<_, i64>(0)
                    )
                    .unwrap(),
                1
            );
            let mut query = reader
                .value()
                .prepare(&format!(
                    "SELECT {} FROM documents WHERE path='page.md'",
                    super::super::normalized_schema::DOCUMENT_COLUMNS
                ))
                .unwrap();
            let mut rows = query.query([]).unwrap();
            let row = document(rows.next().unwrap().unwrap(), 0, |_| Ok(())).unwrap();
            assert_eq!(row.body, format!("{token}\n"));
        }
        assert!(
            !selector::retire(
                &fs,
                &writer,
                &vault,
                &first.identity.selection,
                Duration::ZERO
            )
            .unwrap()
        );
        drop(held);
        assert!(
            selector::retire(
                &fs,
                &writer,
                &vault,
                &first.identity.selection,
                Duration::ZERO
            )
            .unwrap()
        );
        assert!(!first.path.exists());
        assert!(second.path.exists());
    }

    #[test]
    fn reservation_precedes_json_parsing_and_charges_utf8_bytes() {
        let connection = Connection::open_in_memory().unwrap();
        let hash = Blake3Hash::digest("raw");
        let mut statement = connection.prepare(
            "SELECT 'page.md',?1,NULL,NULL,'é','{','', '[]','body','raw',NULL,NULL,'current','[]'"
        ).unwrap();
        let mut cursor = statement.query([hash.as_str()]).unwrap();
        let row = cursor.next().unwrap().unwrap();
        let observed = Cell::new(0);
        let error = document(row, 0, |bytes| {
            observed.set(bytes);
            Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "test reservation",
            ))
        })
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::BudgetExceeded);
        assert_eq!(observed.get(), 7 + 71 + 2 + 1 + 2 + 4 + 3 + 7 + 2);
        assert_eq!(
            document(row, 0, |_| Ok(())).unwrap_err().code,
            ErrorCode::IndexCorrupt
        );
    }

    #[test]
    fn documents_round_trip_unicode_nulls_and_metadata_without_document_json() {
        let connection = Connection::open_in_memory().unwrap();
        let raw = "# Café\nβeta\n";
        let hash = Blake3Hash::digest(raw);
        let mut statement = connection.prepare(
            "SELECT 'sources/a/content.md',?1,NULL,NULL,'Café','[]','Café','[]','Café βeta',?2,'source_a','revision_a','historical','[\"retained\"]'"
        ).unwrap();
        let mut cursor = statement.query(params![hash.as_str(), raw]).unwrap();
        let actual = document(cursor.next().unwrap().unwrap(), 0, |_| Ok(())).unwrap();
        assert_eq!(
            actual,
            DocumentRow {
                path: VaultRelativePath::new("sources/a/content.md").unwrap(),
                hash,
                record_id: None,
                kind: None,
                title: "Café".into(),
                aliases: vec![],
                headings: "Café".into(),
                tags: vec![],
                body: "Café βeta".into(),
                raw_text: raw.into(),
                source_id: Some(RecordId::new("source_a").unwrap()),
                owner_revision: Some(RecordId::new("revision_a").unwrap()),
                eligibility: crate::domain::Eligibility::Historical,
                reasons: vec!["retained".into()],
            }
        );
    }
}

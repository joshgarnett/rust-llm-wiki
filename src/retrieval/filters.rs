//! Bound filters shared by every candidate leg, applied before any cap.
use super::types::*;
use crate::{
    catalog::{DocumentRow, query_types::QueryCatalog},
    domain::*,
};
use rusqlite::types::Value;

pub fn normalize(filters: &SearchFilters) -> Result<SearchFilters> {
    let mut filters = filters.clone();
    filters.kinds.sort();
    filters.kinds.dedup();
    filters.tags.sort();
    filters.tags.dedup();
    filters.source_ids.sort();
    filters.source_ids.dedup();
    filters.authored_statuses.sort();
    filters.authored_statuses.dedup();
    if [
        filters.kinds.len(),
        filters.tags.len(),
        filters.source_ids.len(),
        filters.authored_statuses.len(),
    ]
    .into_iter()
    .any(|length| length > 80)
    {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "each filter list permits at most 80 unique items",
        ));
    }
    if filters
        .tags
        .iter()
        .chain(&filters.authored_statuses)
        .any(|value| value.is_empty() || value.len() > 4096)
    {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "filter values must be nonempty and at most 4096 bytes",
        ));
    }
    if let Some(prefix) = &filters.path_prefix {
        if prefix.is_empty() {
            filters.path_prefix = None;
        } else {
            VaultRelativePath::new(prefix.trim_end_matches('/'))?;
        }
    }
    Ok(filters)
}
pub(crate) fn bind(values: &mut Vec<Value>, value: Value) -> String {
    values.push(value);
    format!("?{}", values.len())
}
pub(crate) fn sql(filters: &SearchFilters, values: &mut Vec<Value>) -> String {
    catalog_sql(filters, values, false)
}
pub(crate) fn catalog_sql(
    filters: &SearchFilters,
    values: &mut Vec<Value>,
    normalized: bool,
) -> String {
    // Ordinary editor notes have no adopted owner. Physical inspection remains
    // available, but only explicit canonical adoption/capture admits retrieval.
    // This predicate runs before candidate limits, including historical modes.
    let mut clauses = vec![
        "(d.record_id IS NOT NULL OR d.source_id IS NOT NULL OR d.owner_revision IS NOT NULL)"
            .to_owned(),
    ];
    if !filters.kinds.is_empty() {
        let placeholders: Vec<_> = filters
            .kinds
            .iter()
            .map(|kind| bind(values, Value::Text(kind.to_string())))
            .collect();
        clauses.push(format!("d.kind IN ({})", placeholders.join(",")));
    }
    for tag in &filters.tags {
        let bound = bind(values, Value::Text(tag.clone()));
        let tags = if normalized {
            "json_each(d.tags_json)"
        } else {
            "json_each(d.row_json,'$.tags')"
        };
        clauses.push(format!("EXISTS(SELECT 1 FROM {tags} WHERE value={bound})"));
    }
    if let Some(prefix) = &filters.path_prefix {
        let bound = bind(values, Value::Text(prefix.clone()));
        clauses.push(format!("substr(d.path,1,length({bound}))={bound}"));
    }
    if !filters.authored_statuses.is_empty() {
        let placeholders: Vec<_> = filters
            .authored_statuses
            .iter()
            .map(|status| bind(values, Value::Text(status.clone())))
            .collect();
        clauses.push(format!("r.authored_status IN ({})", placeholders.join(",")));
    }
    if !filters.source_ids.is_empty() {
        let placeholders: Vec<_> = filters
            .source_ids
            .iter()
            .map(|id| bind(values, Value::Text(id.as_str().into())))
            .collect();
        let ids = placeholders.join(",");
        let evidence = if normalized {
            format!(
                "source_evidence e INDEXED BY source_assertions WHERE e.source_id IN ({ids}) AND e.assertion_id=d.record_id"
            )
        } else {
            format!(
                "evidence e WHERE e.gen=d.gen AND e.assertion_id=d.record_id AND e.source_id IN ({ids})"
            )
        };
        clauses.push(format!("(d.source_id IN ({ids}) OR (r.kind='source' AND r.id IN ({ids})) OR json_extract(r.row_json,'$.record.wiki_source_id') IN ({ids}) OR EXISTS(SELECT 1 FROM json_each(r.row_json,'$.record.wiki_source_ids') WHERE value IN ({ids})) OR EXISTS(SELECT 1 FROM {evidence}))"));
    }
    if clauses.is_empty() {
        "1".into()
    } else {
        clauses.join(" AND ")
    }
}
/// Identity ranking and ordinary text ranking are independent eligibility channels.
pub(crate) fn normal_policy(filters: &SearchFilters) -> String {
    let proposed = if filters.include_proposed {
        "1"
    } else {
        "coalesce(r.authored_status,'')<>'proposed'"
    };
    let state = if filters.include_historical {
        "1".into()
    } else {
        format!(
            "(d.eligibility IN ('current','invalid') AND (coalesce(d.kind,'')<>'entity' OR json_extract(r.row_json,'$.description_eligibility')='current')){}",
            if filters.include_proposed {
                " OR (d.kind='assertion' AND r.authored_status='proposed')"
            } else {
                ""
            }
        )
    };
    // Guard old index snapshots and malformed managed artifacts before any
    // candidate limit. Readable declarations cover copied operational notes.
    let operational = if filters.include_historical {
        "1"
    } else {
        "coalesce(d.kind,'') NOT IN ('extraction_packet','extraction','run','run_event','change','decision') \
         AND d.path NOT GLOB 'knowledge/extractions/packets/*' \
         AND d.path NOT GLOB 'knowledge/extractions/extraction_*.md' \
         AND d.path NOT GLOB 'runs/*/outputs/run_event_generation_*.md' \
         AND d.path NOT GLOB 'runs/*/events/run_event_*.md' \
         AND d.path NOT GLOB 'runs/*/run.md' \
         AND d.path NOT GLOB 'changes/*/change.md'"
    };
    format!("({proposed}) AND ({state}) AND ({operational})")
}
pub(crate) fn identity_policy() -> &'static str {
    "d.kind='entity' AND json_extract(r.row_json,'$.identity_eligibility')='current' AND d.eligibility<>'current'"
}
pub(crate) fn catalog_normal_policy(filters: &SearchFilters, normalized: bool) -> String {
    let policy = normal_policy(filters);
    if normalized {
        policy.replace(
            "json_extract(r.row_json,'$.description_eligibility')",
            "r.description_eligibility",
        )
    } else {
        policy
    }
}
pub(crate) fn catalog_identity_policy(normalized: bool) -> &'static str {
    if normalized {
        "d.kind='entity' AND r.identity_eligibility='current' AND d.eligibility<>'current'"
    } else {
        identity_policy()
    }
}
pub(crate) fn catalog_context_policy(historical: bool, normalized: bool) -> String {
    let policy = context_policy(historical);
    if normalized {
        policy.replace(
            "json_extract(r.row_json,'$.description_eligibility')",
            "r.description_eligibility",
        )
    } else {
        policy
    }
}

/// Revalidate one selected context candidate against exactly the same filter
/// predicate used before search caps, without constructing a full projection.
pub(crate) fn matches_catalog_document(
    reader: &dyn QueryCatalog,
    document: &DocumentRow,
    filters: &SearchFilters,
) -> Result<bool> {
    let filters = normalize(filters)?;
    let normalized = reader.normalized_layout();
    let mut values = vec![Value::Text(document.path.as_str().to_owned())];
    let generation = if normalized {
        String::new()
    } else {
        let generation = i64::try_from(reader.snapshot().generation)
            .map_err(|_| WikiError::new(ErrorCode::IndexCorrupt, "generation outside SQL range"))?;
        let bound = bind(&mut values, Value::Integer(generation));
        format!("AND d.gen={bound}")
    };
    let common = catalog_sql(&filters, &mut values, normalized);
    let join = if normalized {
        "r.id=d.record_id"
    } else {
        "r.gen=d.gen AND r.id=d.record_id"
    };
    let sql = format!(
        "SELECT EXISTS(SELECT 1 FROM documents d INDEXED BY sqlite_autoindex_documents_1 LEFT JOIN records r ON {join} WHERE d.path=?1 {generation} AND ({common}))"
    );
    reader
        .connection()
        .query_row(&sql, rusqlite::params_from_iter(values), |row| row.get(0))
        .map_err(crate::catalog::sql::sql_error)
}
/// Mirrors context's canonical authority rules before SQL candidate limits.
pub(crate) fn context_policy(historical: bool) -> String {
    let payload = if historical {
        "d.eligibility<>'invalid'"
    } else {
        "d.eligibility='current'"
    };
    let page = if historical {
        "r.authored_status<>'draft' AND r.eligibility<>'invalid'"
    } else {
        "r.authored_status='reviewed' AND r.eligibility='current'"
    };
    let description = if historical {
        "json_extract(r.row_json,'$.description_eligibility') IN ('current','historical','stale')"
    } else {
        "json_extract(r.row_json,'$.description_eligibility')='current'"
    };
    format!(
        "((d.owner_revision IS NOT NULL AND ({payload})) OR (d.owner_revision IS NULL AND ((r.kind='page' AND ({page})) OR (r.kind='entity' AND ({description})))))"
    )
}

#[cfg(test)]
mod adoption_tests {
    use super::*;
    #[test]
    fn unadopted_plain_and_unknown_yaml_never_consume_retrieval_caps() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE documents(path TEXT,record_id TEXT,kind TEXT,source_id TEXT,owner_revision TEXT,eligibility TEXT); CREATE TABLE records(id TEXT,kind TEXT,authored_status TEXT,row_json TEXT,description_eligibility TEXT);
            INSERT INTO records VALUES('Page.Good','page','reviewed','{}',NULL);
            INSERT INTO documents VALUES('pages/good.md','Page.Good','page',NULL,NULL,'current');
            INSERT INTO documents VALUES('notes/plain.md',NULL,NULL,NULL,NULL,'unsupported');
            INSERT INTO documents VALUES('notes/unknown-yaml.md',NULL,NULL,NULL,NULL,'invalid');
            INSERT INTO documents VALUES('sources/captured-current/content.md',NULL,NULL,'Source.Current','Revision.Current','current');
            INSERT INTO documents VALUES('sources/captured-historical/content.md',NULL,NULL,'Source.Current','Revision.Old','historical');").unwrap();
        for historical in [false, true] {
            let filters = SearchFilters {
                include_historical: historical,
                ..Default::default()
            };
            let common = catalog_sql(&filters, &mut Vec::new(), true);
            let policy = catalog_normal_policy(&filters, true);
            let sql = format!(
                "SELECT d.path FROM documents d LEFT JOIN records r ON r.id=d.record_id WHERE ({common}) AND ({policy}) ORDER BY d.path LIMIT 10"
            );
            let actual = connection
                .prepare(&sql)
                .unwrap()
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap();
            assert!(actual.contains(&"pages/good.md".to_owned()));
            assert!(actual.contains(&"sources/captured-current/content.md".to_owned()));
            assert_eq!(
                actual.contains(&"sources/captured-historical/content.md".to_owned()),
                historical
            );
            assert!(!actual.iter().any(|path| path.starts_with("notes/")));
        }
    }
}

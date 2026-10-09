//! Read-only accounting for the authenticated GraphV2 row envelope.
//!
//! This is not semantic admission or publication authority. The caller must
//! authenticate the graph projection and retain its pinned catalog selection.
//! Keep the scans in apply order: repeated record loads are separate charges,
//! including loads of rows that do not exist until the record upserts run.
use super::{
    eligibility_facts::EligibilityBaseline,
    normalized_delta::{
        CatalogDelta, DeltaStats, DeltaWriteAllowance, DocumentMutation, GRAPH_V2_MAX_ROWS,
        MAX_DELTA_BYTES, MAX_ROW_BYTES, counted,
    },
    normalized_fact_delta::charge_old,
    policy_facts::{POLICY_LAYOUT, PolicyKind, PolicyRow},
    sql,
    structural_rules::StructuralFact,
    types::{DocumentRow, RecordRow},
};
use crate::{
    domain::{Blake3Hash, ErrorCode, RecordId, RecordKind, Result, VaultRelativePath, WikiError},
    records::RegistryEntry,
    vault::ExpectedState,
};
use rusqlite::{Connection, OptionalExtension, Row, params, types::ValueRef};
use std::collections::{BTreeMap, BTreeSet};

fn bad(message: &str) -> WikiError {
    WikiError::new(ErrorCode::IndexCorrupt, message)
}
fn conflict(message: &str) -> WikiError {
    WikiError::new(ErrorCode::ContentConflict, message)
}
fn budget() -> WikiError {
    WikiError::new(
        ErrorCode::BudgetExceeded,
        "graph preflight exceeds row or byte allowance",
    )
}
fn graph_kind(kind: RecordKind) -> bool {
    matches!(
        kind,
        RecordKind::Entity
            | RecordKind::Assertion
            | RecordKind::Evidence
            | RecordKind::Extraction
            | RecordKind::ExtractionPacket
            | RecordKind::Decision
    )
}

/// Enumerates every charged before-image and anticipated post-upsert load.
/// No savepoint, SQL mutation, transaction control, or catalog copy is used.
pub(super) fn preflight_graph_rows(c: &Connection, delta: &CatalogDelta) -> Result<DeltaStats> {
    let new_rows = delta.validate_with_allowance(DeltaWriteAllowance::authenticated_graph_v2())?;
    delta.require_layout(c)?;
    if !delta.revisions.is_empty()
        || !delta.owners.is_empty()
        || delta.documents.iter().any(|d| {
            matches!(
                d,
                DocumentMutation::MovePage { .. } | DocumentMutation::DeletePage { .. }
            )
        })
    {
        return Err(bad(
            "graph preflight excludes Page moves, deletions and revision mutations",
        ));
    }
    let facts = delta
        .facts
        .as_ref()
        .ok_or_else(|| bad("graph preflight requires facts"))?;
    let policy = facts
        .policy
        .as_ref()
        .ok_or_else(|| bad("graph preflight requires complete policy"))?;
    let mut stats = DeltaStats {
        old_row_limit: GRAPH_V2_MAX_ROWS.checked_sub(new_rows).ok_or_else(budget)?,
        ..DeltaStats::default()
    };

    // FactDelta::check_before loads each current record once. Graph semantic
    // admission owns permitted canonical edits; other kinds are derived only.
    for row in &delta.records {
        let old = load_record(c, row.record.id(), &mut stats)?;
        let occupied: bool = c
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM records WHERE path=?1 AND id<>?2)",
                params![row.path.as_str(), row.record.id().as_str()],
                |r| r.get(0),
            )
            .map_err(sql::sql_error)?;
        if occupied {
            return Err(conflict("graph record path has another adopted owner"));
        }
        match &old {
            Some(old) if old.path != row.path || old.record.kind() != row.record.kind() => {
                return Err(conflict("graph record identity, path or kind changed"));
            }
            Some(old)
                if !graph_kind(row.record.kind())
                    && (old.record != row.record || old.hash != row.hash) =>
            {
                return Err(bad(
                    "non-graph canonical record mutation is outside graph scope",
                ));
            }
            None if !graph_kind(row.record.kind()) => {
                return Err(bad("new non-graph canonical record is outside graph scope"));
            }
            _ => {}
        }
        if !facts
            .records
            .iter()
            .any(|f| f.record_id == *row.record.id())
        {
            require_fact(c, delta, row.record.id())?;
        }
        if old.is_none()
            && (!facts
                .records
                .iter()
                .any(|f| f.record_id == *row.record.id())
                || !facts
                    .registry
                    .iter()
                    .any(|r| r.record_id == *row.record.id()))
        {
            return Err(bad("new graph owner lacks fact or registry replacement"));
        }
    }

    // normalized_delta::put_document / put_graph charge only old FTS columns.
    for action in &delta.documents {
        match action {
            DocumentMutation::Put { row } => preflight_document(c, delta, row, &mut stats)?,
            DocumentMutation::Metadata { path, .. } => require_document(c, delta, path)?,
            _ => unreachable!("scope checked above"),
        }
    }
    for graph in &delta.graph {
        let kind = anticipated_kind(c, delta, &graph.target_id)?;
        if kind != graph.target_kind {
            return Err(bad("graph row differs from adopted owner kind"));
        }
        let mut s = c.prepare("SELECT graph_row,name,aliases_text,endpoints,predicate,qualifiers,description,target_kind,target_id FROM graph_rows WHERE target_id=?1").map_err(sql::sql_error)?;
        let mut rows = s
            .query([graph.target_id.as_str()])
            .map_err(sql::sql_error)?;
        if let Some(old) = rows.next().map_err(sql::sql_error)? {
            old.get::<_, i64>(0).map_err(sql::sql_error)?;
            charge_fts(old, 1..9, &mut stats)?;
            if text(old, 7)? != graph.target_kind.as_str()
                || text(old, 8)? != graph.target_id.as_str()
            {
                return Err(conflict("graph target identity or kind changed"));
            }
        }
    }
    // These apply scans count rows, not bytes. Deliberately no LIMIT 4097:
    // every row removed by a GraphV2 owner replacement must be represented.
    for owned in &delta.links {
        require_document(c, delta, &owned.path)?;
        count_owned(
            c,
            "SELECT link_row FROM links WHERE from_path=?1",
            owned.path.as_str(),
            true,
            &mut stats,
        )?;
    }
    for owned in &delta.diagnostics {
        require_document(c, delta, &owned.path)?;
        count_owned(
            c,
            "SELECT diagnostic_row FROM diagnostics WHERE path=?1",
            owned.path.as_str(),
            true,
            &mut stats,
        )?;
    }
    for owned in &delta.claims {
        require_document(c, delta, &owned.path)?;
        count_owned(
            c,
            "SELECT record_id FROM identity_claims INDEXED BY identity_claim_paths WHERE path=?1",
            owned.path.as_str(),
            false,
            &mut stats,
        )?;
    }

    // FactDelta::apply now sees the upserted records, not their before-images.
    for item in &facts.records {
        let row = post_record(c, delta, &item.record_id, &mut stats)?;
        if !item.fact.direct_paths.contains(&row.path) {
            return Err(bad("fact lacks own canonical path"));
        }
        for path in &item.fact.direct_paths {
            if path != &row.path && crate::sources::revision::canonical_path(path) {
                return Err(bad("fact captures related canonical note"));
            }
            require_dependency(c, delta, path, (path == &row.path).then_some(&row.hash))?;
        }
        scan_old(
            c,
            "SELECT record_id,baseline_json,structural_json FROM record_eligibility_facts WHERE record_id=?1",
            item.record_id.as_str(),
            3,
            &mut stats,
            |r| {
                if text(r, 0)? != item.record_id.as_str() {
                    return Err(bad("fact owner mismatch"));
                }
                decode::<EligibilityBaseline>(text(r, 1)?)?;
                decode::<StructuralFact>(text(r, 2)?)?;
                Ok(())
            },
        )?;
        scan_old(
            c,
            "SELECT owner_id,path FROM record_direct_paths WHERE owner_id=?1",
            item.record_id.as_str(),
            2,
            &mut stats,
            |r| {
                if text(r, 0)? != item.record_id.as_str() {
                    return Err(bad("direct path owner mismatch"));
                }
                valid_path(text(r, 1)?)?;
                Ok(())
            },
        )?;
    }
    for edge in &facts.edge_deletes {
        require_fact(c, delta, &edge.owner_id)?;
        charge_old(counted(edge, MAX_ROW_BYTES)?, &mut stats)?;
        let present: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM semantic_edges WHERE owner_id=?1 AND target_id=?2 AND role_json=?3)", params![edge.owner_id.as_str(), edge.target_id.as_str(), sql::json(&edge.role)?], |r| r.get(0)).map_err(sql::sql_error)?;
        if !present {
            return Err(conflict("exact semantic edge deletion is absent"));
        }
    }
    for edge in &facts.edge_inserts {
        require_fact(c, delta, &edge.owner_id)?;
        let present: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM semantic_edges WHERE owner_id=?1 AND target_id=?2 AND role_json=?3)", params![edge.owner_id.as_str(), edge.target_id.as_str(), sql::json(&edge.role)?], |r| r.get(0)).map_err(sql::sql_error)?;
        if present {
            return Err(conflict("semantic edge insertion already exists"));
        }
    }
    for owned in &facts.links {
        scan_old(
            c,
            "SELECT from_path,byte_start,raw_destination,typed_id,typed_kind FROM link_facts WHERE from_path=?1",
            owned.path.as_str(),
            5,
            &mut stats,
            |r| {
                if text(r, 0)? != owned.path.as_str() {
                    return Err(bad("raw link owner mismatch"));
                }
                offset(r, 1)?;
                text(r, 2)?;
                let id: Option<String> = r.get(3).map_err(sql::sql_error)?;
                let kind: Option<String> = r.get(4).map_err(sql::sql_error)?;
                match (id, kind) {
                    (Some(id), Some(kind)) => {
                        valid_id(&id)?;
                        valid_kind(&kind)?;
                    }
                    (None, None) => {}
                    _ => return Err(bad("incomplete raw link typed identity")),
                }
                Ok(())
            },
        )?;
        scan_old(
            c,
            "SELECT kind,value,from_path,byte_start FROM link_match_keys INDEXED BY link_match_owners WHERE from_path=?1",
            owned.path.as_str(),
            4,
            &mut stats,
            |r| {
                match_key(r)?;
                if text(r, 2)? != owned.path.as_str() {
                    return Err(bad("link key owner mismatch"));
                }
                offset(r, 3)?;
                let present: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM link_facts WHERE from_path=?1 AND byte_start=?2)", params![owned.path.as_str(), r.get::<_, i64>(3).map_err(sql::sql_error)?], |r| r.get(0)).map_err(sql::sql_error)?;
                if !present {
                    return Err(bad("orphan raw link match key"));
                }
                Ok(())
            },
        )?;
    }
    for entry in &facts.registry {
        let row = post_record(c, delta, &entry.record_id, &mut stats)?;
        let expected = super::link_facts::registry_keys(&RegistryEntry {
            id: entry.record_id.clone(),
            path: row.path.clone(),
            kind: row.record.kind(),
            aliases: super::scan::list(&row.record, "aliases"),
        })?;
        if entry.path != row.path || entry.keys != expected {
            return Err(bad("registry keys differ from adopted record"));
        }
        scan_old(
            c,
            "SELECT kind,value,record_id,path FROM registry_match_keys INDEXED BY registry_match_owners WHERE record_id=?1",
            entry.record_id.as_str(),
            4,
            &mut stats,
            |r| {
                match_key(r)?;
                if text(r, 2)? != entry.record_id.as_str() || text(r, 3)? != row.path.as_str() {
                    return Err(bad("registry key owner identity mismatch"));
                }
                Ok(())
            },
        )?;
    }

    // PolicyDelta::apply always charges layout, then each replacement family.
    let layout = policy_scan(c, "family=?1", "layout", &mut stats)?;
    if layout != vec![PolicyRow::Layout(POLICY_LAYOUT)] {
        return Err(bad("foreign normalized policy layout"));
    }
    for update in &policy.memberships {
        policy_scan(
            c,
            "family='membership' AND owner=?1",
            update.path.as_str(),
            &mut stats,
        )?;
    }
    for replacement in &policy.replacements {
        let kind = replacement.kind.name();
        for predicate in [
            "family='state' AND key=?1",
            "family='dependency' AND owner=?1",
            "family='read_path' AND owner=?1",
        ] {
            policy_scan(c, predicate, kind, &mut stats)?;
        }
        policy_scan(
            c,
            "family=?1",
            if replacement.kind == PolicyKind::Remap {
                "remap_edge"
            } else {
                "review_edge"
            },
            &mut stats,
        )?;
        if replacement.kind == PolicyKind::Remap {
            policy_scan(c, "family=?1", "alias", &mut stats)?;
            let rows = policy_scan(c, "family=?1", "family_member", &mut stats)?;
            let mut groups: BTreeMap<Blake3Hash, BTreeSet<RecordId>> = BTreeMap::new();
            for row in rows {
                if let PolicyRow::FamilyMember { family, id } = row {
                    groups.entry(family).or_default().insert(id);
                }
            }
            for (family, ids) in groups {
                if Blake3Hash::digest(sql::json(&ids)?.as_bytes()) != family {
                    return Err(bad("policy family hash differs from exact members"));
                }
            }
        }
    }
    // Apply's final presence checks inspect anticipated fact/direct/registry
    // relations without additional charged row loads.
    for row in &delta.records {
        require_fact(c, delta, row.record.id())?;
        let own = if let Some(item) = facts
            .records
            .iter()
            .find(|f| f.record_id == *row.record.id())
        {
            item.fact.direct_paths.contains(&row.path)
        } else {
            c.query_row(
                "SELECT EXISTS(SELECT 1 FROM record_direct_paths WHERE owner_id=?1 AND path=?2)",
                params![row.record.id().as_str(), row.path.as_str()],
                |r| r.get(0),
            )
            .map_err(sql::sql_error)?
        };
        if !own {
            return Err(bad("changed graph record lacks exact own state"));
        }
        require_dependency(c, delta, &row.path, Some(&row.hash))?;
        let registered = if let Some(entry) = facts
            .registry
            .iter()
            .find(|e| e.record_id == *row.record.id())
        {
            entry.path == row.path
                && entry.keys.iter().any(|k| {
                    k.kind == super::link_facts::MatchKeyKind::Id
                        && k.value == row.record.id().as_str()
                })
        } else {
            c.query_row("SELECT EXISTS(SELECT 1 FROM registry_match_keys WHERE kind='id' AND value=?1 AND record_id=?1 AND path=?2)", params![row.record.id().as_str(), row.path.as_str()], |r| r.get(0)).map_err(sql::sql_error)?
        };
        if !registered {
            return Err(bad("changed graph record lacks registry identity"));
        }
    }
    stats.records = delta.records.len();
    stats.documents = delta.documents.len();
    stats.graph_rows = delta.graph.len();
    stats.links = delta.links.iter().map(|o| o.rows.len()).sum();
    stats.diagnostics = delta.diagnostics.iter().map(|o| o.rows.len()).sum();
    stats.claims = delta.claims.iter().map(|o| o.rows.len()).sum();
    stats.dependencies = delta.dependencies.len();
    stats.fact_rows = facts
        .records
        .iter()
        .map(|r| 1 + r.fact.direct_paths.len())
        .sum::<usize>()
        + facts.edge_deletes.len()
        + facts.edge_inserts.len()
        + facts
            .links
            .iter()
            .flat_map(|o| &o.rows)
            .map(|r| 1 + r.keys.len())
            .sum::<usize>()
        + facts.registry.iter().map(|r| r.keys.len()).sum::<usize>()
        + policy
            .memberships
            .iter()
            .map(|m| m.keys.len())
            .sum::<usize>()
        + policy
            .replacements
            .iter()
            .map(|r| r.rows.len())
            .sum::<usize>();
    Ok(stats)
}

fn text<'a>(row: &'a Row<'_>, column: usize) -> Result<&'a str> {
    row.get_ref(column)
        .map_err(sql::sql_error)?
        .as_str()
        .map_err(|_| bad("graph preflight requires UTF-8 text"))
}
fn decode<T: serde::de::DeserializeOwned>(value: &str) -> Result<T> {
    serde_json::from_str(value).map_err(|_| bad("malformed normalized before-image JSON"))
}
fn valid_id(value: &str) -> Result<RecordId> {
    RecordId::new(value).map_err(|_| bad("invalid before-image record ID"))
}
fn valid_path(value: &str) -> Result<VaultRelativePath> {
    VaultRelativePath::new(value).map_err(|_| bad("invalid before-image path"))
}
fn valid_kind(value: &str) -> Result<RecordKind> {
    value
        .parse()
        .map_err(|_| bad("invalid before-image record kind"))
}
fn offset(row: &Row<'_>, column: usize) -> Result<()> {
    let n: i64 = row.get(column).map_err(sql::sql_error)?;
    if n < 0 {
        return Err(bad("negative before-image link offset"));
    }
    Ok(())
}
fn match_key(row: &Row<'_>) -> Result<()> {
    if !matches!(text(row, 0)?, "id" | "path" | "basename" | "alias") {
        return Err(bad("unknown before-image match key kind"));
    }
    text(row, 1)?;
    Ok(())
}
fn add(a: usize, b: usize) -> Result<usize> {
    a.checked_add(b).ok_or_else(budget)
}
fn charge_count(stats: &mut DeltaStats) -> Result<()> {
    stats.old_rows = stats
        .old_rows
        .checked_add(1)
        .filter(|n| *n <= stats.old_row_limit)
        .ok_or_else(budget)?;
    Ok(())
}
fn charge_fts(
    row: &Row<'_>,
    columns: std::ops::Range<usize>,
    stats: &mut DeltaStats,
) -> Result<()> {
    let mut bytes = 0;
    for column in columns {
        bytes = add(bytes, text(row, column)?.len())?;
    }
    if bytes > MAX_ROW_BYTES {
        return Err(budget());
    }
    stats.old_fts_bytes = add(stats.old_fts_bytes, bytes)?;
    if add(stats.old_fts_bytes, stats.old_fact_bytes)? > MAX_DELTA_BYTES {
        return Err(budget());
    }
    charge_count(stats)
}
fn count_owned(
    c: &Connection,
    query: &str,
    key: &str,
    integer: bool,
    stats: &mut DeltaStats,
) -> Result<()> {
    let mut s = c.prepare(query).map_err(sql::sql_error)?;
    let mut rows = s.query([key]).map_err(sql::sql_error)?;
    while let Some(row) = rows.next().map_err(sql::sql_error)? {
        if integer {
            row.get::<_, i64>(0).map_err(sql::sql_error)?;
        } else {
            valid_id(text(row, 0)?)?;
        }
        charge_count(stats)?;
    }
    Ok(())
}
fn scan_old(
    c: &Connection,
    query: &str,
    key: &str,
    columns: usize,
    stats: &mut DeltaStats,
    mut validate: impl FnMut(&Row<'_>) -> Result<()>,
) -> Result<()> {
    let mut s = c.prepare(query).map_err(sql::sql_error)?;
    let mut rows = s.query([key]).map_err(sql::sql_error)?;
    while let Some(row) = rows.next().map_err(sql::sql_error)? {
        let mut bytes = 0;
        for column in 0..columns {
            bytes = add(
                bytes,
                match row.get_ref(column).map_err(sql::sql_error)? {
                    ValueRef::Null => 0,
                    ValueRef::Integer(_) | ValueRef::Real(_) => 8,
                    ValueRef::Text(value) | ValueRef::Blob(value) => value.len(),
                },
            )?;
        }
        charge_old(bytes, stats)?;
        validate(row)?;
    }
    Ok(())
}
fn load_record(c: &Connection, id: &RecordId, stats: &mut DeltaStats) -> Result<Option<RecordRow>> {
    let mut s = c
        .prepare("SELECT id,kind,path,hash,row_json FROM records WHERE id=?1")
        .map_err(sql::sql_error)?;
    let mut rows = s.query([id.as_str()]).map_err(sql::sql_error)?;
    let Some(row) = rows.next().map_err(sql::sql_error)? else {
        return Ok(None);
    };
    let mut bytes = 0;
    for column in 0..5 {
        bytes = add(bytes, text(row, column)?.len())?;
    }
    charge_old(bytes, stats)?;
    let decoded: RecordRow = decode(text(row, 4)?)?;
    if decoded.record.id() != id
        || text(row, 0)? != id.as_str()
        || text(row, 1)? != decoded.record.kind().as_str()
        || text(row, 2)? != decoded.path.as_str()
        || text(row, 3)? != decoded.hash.as_str()
    {
        return Err(bad("record JSON differs from normalized identity columns"));
    }
    Ok(Some(decoded))
}
fn post_record(
    c: &Connection,
    delta: &CatalogDelta,
    id: &RecordId,
    stats: &mut DeltaStats,
) -> Result<RecordRow> {
    if let Some(row) = delta.records.iter().find(|r| r.record.id() == id) {
        // Exact columns written to records, including SQL's compact row_json.
        let bytes = [
            id.as_str().len(),
            row.record.kind().as_str().len(),
            row.path.as_str().len(),
            row.hash.as_str().len(),
            counted(row, MAX_ROW_BYTES)?,
        ]
        .into_iter()
        .try_fold(0, add)?;
        charge_old(bytes, stats)?;
        return Ok(row.clone());
    }
    load_record(c, id, stats)?.ok_or_else(|| bad("orphan anticipated fact or registry owner"))
}
fn anticipated_kind(c: &Connection, delta: &CatalogDelta, id: &RecordId) -> Result<RecordKind> {
    if let Some(row) = delta.records.iter().find(|r| r.record.id() == id) {
        return Ok(row.record.kind());
    }
    let kind: Option<String> = c
        .query_row("SELECT kind FROM records WHERE id=?1", [id.as_str()], |r| {
            r.get(0)
        })
        .optional()
        .map_err(sql::sql_error)?;
    valid_kind(&kind.ok_or_else(|| bad("orphan graph owner"))?)
}
fn require_fact(c: &Connection, delta: &CatalogDelta, id: &RecordId) -> Result<()> {
    anticipated_kind(c, delta, id)?;
    if delta
        .facts
        .as_ref()
        .is_some_and(|f| f.records.iter().any(|r| &r.record_id == id))
    {
        return Ok(());
    }
    let present: bool = c
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM record_eligibility_facts WHERE record_id=?1)",
            [id.as_str()],
            |r| r.get(0),
        )
        .map_err(sql::sql_error)?;
    if !present {
        return Err(bad("normalized fact or adopted owner missing"));
    }
    Ok(())
}
fn require_document(c: &Connection, delta: &CatalogDelta, path: &VaultRelativePath) -> Result<()> {
    if delta
        .documents
        .iter()
        .any(|d| matches!(d, DocumentMutation::Put { row } if &row.path == path))
    {
        return Ok(());
    }
    let present: bool = c
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM documents WHERE path=?1)",
            [path.as_str()],
            |r| r.get(0),
        )
        .map_err(sql::sql_error)?;
    if !present {
        return Err(conflict("graph document owner missing"));
    }
    Ok(())
}
fn require_dependency(
    c: &Connection,
    delta: &CatalogDelta,
    path: &VaultRelativePath,
    own_hash: Option<&Blake3Hash>,
) -> Result<()> {
    if let Some(dep) = delta.dependencies.iter().find(|d| &d.path == path) {
        if own_hash.is_none()
            || matches!((&dep.expected, own_hash), (ExpectedState::Hash(actual), Some(expected)) if actual == expected)
        {
            return Ok(());
        }
        return Err(bad("fact path lacks exact anticipated dependency"));
    }
    let hash: Option<Option<String>> = c
        .query_row(
            "SELECT expected_hash FROM dependencies WHERE path=?1",
            [path.as_str()],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql::sql_error)?;
    if hash.is_some()
        && own_hash.is_none_or(|expected| {
            hash.as_ref().and_then(|h| h.as_deref()) == Some(expected.as_str())
        })
    {
        return Ok(());
    }
    Err(bad("fact path lacks exact post-delta state"))
}
fn preflight_document(
    c: &Connection,
    delta: &CatalogDelta,
    row: &DocumentRow,
    stats: &mut DeltaStats,
) -> Result<()> {
    if let Some(id) = &row.record_id {
        if row.kind != Some(anticipated_kind(c, delta, id)?) {
            return Err(bad("document differs from adopted owner kind"));
        }
        let exact = if let Some(record) = delta.records.iter().find(|r| r.record.id() == id) {
            record.path == row.path && record.hash == row.hash
        } else {
            c.query_row(
                "SELECT EXISTS(SELECT 1 FROM records WHERE id=?1 AND path=?2 AND hash=?3)",
                params![id.as_str(), row.path.as_str(), row.hash.as_str()],
                |r| r.get(0),
            )
            .map_err(sql::sql_error)?
        };
        if !exact {
            return Err(bad("document differs from anticipated owner path or hash"));
        }
        let occupied: bool = c
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM documents WHERE record_id=?1 AND path<>?2)",
                params![id.as_str(), row.path.as_str()],
                |r| r.get(0),
            )
            .map_err(sql::sql_error)?;
        if occupied {
            return Err(conflict(
                "document record identity already has another path",
            ));
        }
    }
    let mut s = c.prepare("SELECT doc_row,title,aliases_text,headings,tags_text,body,record_id IS ?2 AND kind IS ?3 AND source_id IS ?4 AND owner_revision IS ?5,file_hash IS ?6 AND body IS ?7 AND raw_text IS ?8 FROM documents WHERE path=?1").map_err(sql::sql_error)?;
    let mut rows = s
        .query(params![
            row.path.as_str(),
            row.record_id.as_ref().map(RecordId::as_str),
            row.kind.map(RecordKind::as_str),
            row.source_id.as_ref().map(RecordId::as_str),
            row.owner_revision.as_ref().map(RecordId::as_str),
            row.hash.as_str(),
            row.body,
            row.raw_text
        ])
        .map_err(sql::sql_error)?;
    if let Some(old) = rows.next().map_err(sql::sql_error)? {
        old.get::<_, i64>(0).map_err(sql::sql_error)?;
        charge_fts(old, 1..6, stats)?;
        if !old.get::<_, bool>(6).map_err(sql::sql_error)? {
            return Err(conflict("document identity or ownership changed"));
        }
        if row.kind.is_none_or(|kind| !graph_kind(kind))
            && !old.get::<_, bool>(7).map_err(sql::sql_error)?
        {
            return Err(bad("non-graph document bytes changed"));
        }
    } else if row.kind.is_none_or(|kind| !graph_kind(kind))
        || row.record_id.is_none()
        || row.source_id.is_some()
        || row.owner_revision.is_some()
    {
        return Err(bad("new document is outside canonical graph scope"));
    }
    Ok(())
}
fn policy_scan(
    c: &Connection,
    predicate: &str,
    key: &str,
    stats: &mut DeltaStats,
) -> Result<Vec<PolicyRow>> {
    let index = if predicate.contains("owner=?1") {
        " INDEXED BY policy_facts_owner"
    } else {
        ""
    };
    let mut s = c
        .prepare(&format!(
            "SELECT family,key,owner,value FROM policy_facts{index} WHERE {predicate}"
        ))
        .map_err(sql::sql_error)?;
    let mut rows = s.query([key]).map_err(sql::sql_error)?;
    let mut result = Vec::new();
    while let Some(row) = rows.next().map_err(sql::sql_error)? {
        let mut bytes = 0;
        for column in 0..4 {
            bytes = add(bytes, text(row, column)?.len())?;
        }
        charge_old(bytes, stats)?;
        let columns = [
            text(row, 0)?.to_owned(),
            text(row, 1)?.to_owned(),
            text(row, 2)?.to_owned(),
            text(row, 3)?.to_owned(),
        ];
        result.push(PolicyRow::from_columns(columns)?);
    }
    Ok(result)
}

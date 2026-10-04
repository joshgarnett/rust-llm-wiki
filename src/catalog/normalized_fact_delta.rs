//! Atomic maintenance of normalized source-refresh facts. Structural checks here
//! do not replace the source projector's semantic admission capability.
use super::{
    eligibility_facts::{EligibilityEdge, EligibilityFact},
    link_facts::{MatchKey, MatchKeyKind, OwnedLinkFact},
    normalized_delta::{
        CatalogDelta, DeltaStats, MAX_DELTA_BYTES, MAX_ROW_BYTES, MAX_ROWS, counted,
    },
    sql,
    types::RecordRow,
};
use crate::{
    domain::{ErrorCode, RecordId, RecordKind, Result, VaultRelativePath, WikiError},
    records::RegistryEntry,
};
use rusqlite::{Connection, params, types::ValueRef};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FactDelta {
    pub records: Vec<RecordFactMutation>,
    pub edge_inserts: Vec<EligibilityEdge>,
    pub edge_deletes: Vec<EligibilityEdge>,
    pub links: Vec<OwnedLinkFacts>,
    pub registry: Vec<OwnedRegistryKeys>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecordFactMutation {
    pub record_id: RecordId,
    pub fact: EligibilityFact,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OwnedLinkFacts {
    pub path: VaultRelativePath,
    pub rows: Vec<OwnedLinkFact>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OwnedRegistryKeys {
    pub record_id: RecordId,
    pub path: VaultRelativePath,
    pub keys: Vec<MatchKey>,
}
fn invalid(s: &str) -> WikiError {
    WikiError::new(ErrorCode::IndexCorrupt, s)
}
fn conflict(s: &str) -> WikiError {
    WikiError::new(ErrorCode::ContentConflict, s)
}
fn budget() -> WikiError {
    WikiError::new(
        ErrorCode::BudgetExceeded,
        "normalized fact delta exceeds cumulative allowance",
    )
}
fn unique<T: Ord>(set: &mut BTreeSet<T>, v: T) -> Result<()> {
    if set.insert(v) {
        Ok(())
    } else {
        Err(invalid("duplicate normalized fact mutation"))
    }
}
pub(super) fn key_name(kind: MatchKeyKind) -> &'static str {
    match kind {
        MatchKeyKind::Id => "id",
        MatchKeyKind::Path => "path",
        MatchKeyKind::Basename => "basename",
        MatchKeyKind::Alias => "alias",
    }
}
fn admit_new<T: Serialize>(v: &T, count: &mut usize) -> Result<()> {
    *count = count
        .checked_add(1)
        .filter(|n| *n <= MAX_ROWS)
        .ok_or_else(budget)?;
    counted(v, MAX_ROW_BYTES)?;
    Ok(())
}

impl FactDelta {
    pub(super) fn validate(&self, delta: &CatalogDelta, count: &mut usize) -> Result<()> {
        let mut owners = BTreeSet::new();
        for row in &self.records {
            unique(&mut owners, &row.record_id)?;
            admit_new(
                &(&row.record_id, &row.fact.baseline, &row.fact.structural),
                count,
            )?;
            for path in &row.fact.direct_paths {
                admit_new(&(&row.record_id, path), count)?;
            }
        }
        let mut edges = BTreeSet::new();
        for edge in self.edge_inserts.iter().chain(&self.edge_deletes) {
            unique(&mut edges, edge)?;
            admit_new(edge, count)?;
        }
        let mut paths = BTreeSet::new();
        for owned in &self.links {
            unique(&mut paths, &owned.path)?;
            // Empty owner replacement is still bounded work.
            admit_new(&owned.path, count)?;
            let links = delta
                .links
                .iter()
                .find(|links| links.path == owned.path)
                .ok_or_else(|| invalid("raw link replacement lacks resolved link owner"))?;
            let mut offsets = BTreeSet::new();
            for fact in &owned.rows {
                unique(&mut offsets, fact.byte_start)?;
                if fact.from_path != owned.path {
                    return Err(invalid("cross-owned raw link fact"));
                }
                admit_new(fact, count)?;
                let link = links
                    .rows
                    .iter()
                    .find(|link| link.byte_start == fact.byte_start)
                    .ok_or_else(|| invalid("raw link fact has no resolved offset"))?;
                if link.target_id.is_some() != link.target_path.is_some() {
                    return Err(invalid("resolved link target identity is incomplete"));
                }
                let mut expected = if let Some(target) = &fact.typed {
                    super::link_facts::typed_fact(
                        &owned.path,
                        fact.byte_start,
                        &fact.raw_destination,
                        &target.id,
                        target.expected_kind,
                    )?
                } else {
                    let lookup = crate::records::links::untyped_lookup(&fact.raw_destination);
                    if !matches!(lookup, crate::records::links::UntypedLookup::Local { .. })
                        && link.target_id.is_some()
                    {
                        return Err(invalid(
                            "external or invalid destination has a resolved identity",
                        ));
                    }
                    let resolution = match lookup {
                        crate::records::links::UntypedLookup::External => {
                            crate::records::LinkResolution::External
                        }
                        _ => crate::records::LinkResolution::Missing,
                    };
                    let mut expected = super::link_facts::untyped_fact(
                        &owned.path,
                        fact.byte_start,
                        &fact.raw_destination,
                        &resolution,
                    )?;
                    if let Some(id) = &link.target_id {
                        expected.keys.push(MatchKey {
                            kind: MatchKeyKind::Id,
                            value: id.to_string(),
                        });
                    }
                    expected
                };
                expected.keys.sort();
                if expected.keys != fact.keys {
                    return Err(invalid(
                        "raw link keys differ from resolved identity or destination",
                    ));
                }
                for key in &fact.keys {
                    admit_new(&(&owned.path, fact.byte_start, key), count)?;
                }
            }
            if offsets.len() != links.rows.len() {
                return Err(invalid("raw and resolved link offset sets differ"));
            }
        }
        if paths.len() != delta.links.len() {
            return Err(invalid("resolved links lack normalized raw facts"));
        }
        owners.clear();
        for row in &self.registry {
            unique(&mut owners, &row.record_id)?;
            admit_new(&(&row.record_id, &row.path), count)?;
            let mut keys = BTreeSet::new();
            for key in &row.keys {
                unique(&mut keys, key)?;
                admit_new(&(&row.record_id, &row.path, key), count)?;
            }
        }
        Ok(())
    }

    /// Runs before ordinary row replacement. This v2 delta is restricted to
    /// source refresh: authored propositions and evidence membership stay fixed.
    pub(super) fn check_before(
        &self,
        c: &Connection,
        delta: &CatalogDelta,
        stats: &mut DeltaStats,
    ) -> Result<()> {
        for row in &delta.records {
            match load_record(c, row.record.id(), stats)? {
                Some(old) => {
                    if old.record.kind() != RecordKind::Source
                        && (old.record != row.record || old.hash != row.hash)
                    {
                        return Err(conflict(
                            "source refresh cannot change existing immutable or authored canonical records",
                        ));
                    }
                    if old.record.kind() == RecordKind::Source {
                        let retained = |key: &&String| {
                            !matches!(
                                key.as_str(),
                                "title"
                                    | "wiki_current_revision"
                                    | "wiki_revision"
                                    | "wiki_revisions"
                            )
                        };
                        if old
                            .record
                            .fields()
                            .iter()
                            .filter(|(key, _)| retained(key))
                            .ne(row.record.fields().iter().filter(|(key, _)| retained(key)))
                        {
                            return Err(conflict("source refresh changed fixed source fields"));
                        }
                    }
                    if old.path != row.path || old.record.kind() != row.record.kind() {
                        return Err(conflict("normalized record identity changed"));
                    }
                    if !self
                        .records
                        .iter()
                        .any(|fact| fact.record_id == *row.record.id())
                    {
                        require_fact(c, row.record.id())?;
                    }
                }
                None => {
                    if row.record.kind() != RecordKind::Revision
                        || !self
                            .records
                            .iter()
                            .any(|fact| fact.record_id == *row.record.id())
                        || !self
                            .registry
                            .iter()
                            .any(|entry| entry.record_id == *row.record.id())
                    {
                        return Err(invalid(
                            "new refresh revision requires fact and registry rows",
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    pub(super) fn apply(
        &self,
        c: &Connection,
        delta: &CatalogDelta,
        stats: &mut DeltaStats,
    ) -> Result<()> {
        for item in &self.records {
            let row = load_record(c, &item.record_id, stats)?
                .ok_or_else(|| invalid("orphan normalized fact owner"))?;
            if !item.fact.direct_paths.contains(&row.path) {
                return Err(invalid("fact lacks own canonical path"));
            }
            for path in &item.fact.direct_paths {
                if path != &row.path && crate::sources::revision::canonical_path(path) {
                    return Err(invalid("fact captures related canonical note"));
                }
                let expected:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM dependencies WHERE path=?1 AND (?2=0 OR expected_hash=?3))",params![path.as_str(),path==&row.path,row.hash.as_str()],|r|r.get(0)).map_err(sql::sql_error)?;
                if !expected {
                    return Err(invalid("fact path lacks exact post-delta state"));
                }
            }
            admit_old(
                c,
                "SELECT record_id,baseline_json,structural_json FROM record_eligibility_facts WHERE record_id=?1",
                item.record_id.as_str(),
                3,
                stats,
            )?;
            admit_old(
                c,
                "SELECT owner_id,path FROM record_direct_paths WHERE owner_id=?1",
                item.record_id.as_str(),
                2,
                stats,
            )?;
            c.execute("INSERT INTO record_eligibility_facts VALUES(?1,?2,?3) ON CONFLICT(record_id) DO UPDATE SET baseline_json=excluded.baseline_json,structural_json=excluded.structural_json",params![item.record_id.as_str(),sql::json(&item.fact.baseline)?,sql::json(&item.fact.structural)?]).map_err(sql::sql_error)?;
            c.execute(
                "DELETE FROM record_direct_paths WHERE owner_id=?1",
                [item.record_id.as_str()],
            )
            .map_err(sql::sql_error)?;
            for path in &item.fact.direct_paths {
                c.execute(
                    "INSERT INTO record_direct_paths VALUES(?1,?2)",
                    params![item.record_id.as_str(), path.as_str()],
                )
                .map_err(sql::sql_error)?;
            }
            stats.fact_rows += 1 + item.fact.direct_paths.len();
        }
        for edge in &self.edge_deletes {
            require_fact(c, &edge.owner_id)?;
            charge_old(counted(edge, MAX_ROW_BYTES)?, stats)?;
            let n=c.execute("DELETE FROM semantic_edges WHERE owner_id=?1 AND target_id=?2 AND role_json=?3",params![edge.owner_id.as_str(),edge.target_id.as_str(),sql::json(&edge.role)?]).map_err(sql::sql_error)?;
            if n != 1 {
                return Err(conflict("exact semantic edge deletion is absent"));
            }
            stats.fact_rows += 1;
        }
        for edge in &self.edge_inserts {
            require_fact(c, &edge.owner_id)?;
            c.execute(
                "INSERT INTO semantic_edges VALUES(?1,?2,?3)",
                params![
                    edge.owner_id.as_str(),
                    edge.target_id.as_str(),
                    sql::json(&edge.role)?
                ],
            )
            .map_err(sql::sql_error)?;
            stats.fact_rows += 1;
        }
        for owned in &self.links {
            admit_old(
                c,
                "SELECT from_path,byte_start,raw_destination,typed_id,typed_kind FROM link_facts WHERE from_path=?1",
                owned.path.as_str(),
                5,
                stats,
            )?;
            admit_old(
                c,
                "SELECT kind,value,from_path,byte_start FROM link_match_keys INDEXED BY link_match_owners WHERE from_path=?1",
                owned.path.as_str(),
                4,
                stats,
            )?;
            c.execute(
                "DELETE FROM link_match_keys WHERE from_path=?1",
                [owned.path.as_str()],
            )
            .map_err(sql::sql_error)?;
            c.execute(
                "DELETE FROM link_facts WHERE from_path=?1",
                [owned.path.as_str()],
            )
            .map_err(sql::sql_error)?;
            for fact in &owned.rows {
                c.execute(
                    "INSERT INTO link_facts VALUES(?1,?2,?3,?4,?5)",
                    params![
                        owned.path.as_str(),
                        sql::integer(fact.byte_start)?,
                        fact.raw_destination,
                        fact.typed.as_ref().map(|t| t.id.as_str()),
                        fact.typed.as_ref().map(|t| t.expected_kind.as_str())
                    ],
                )
                .map_err(sql::sql_error)?;
                stats.fact_rows += 1;
                for key in &fact.keys {
                    c.execute(
                        "INSERT INTO link_match_keys VALUES(?1,?2,?3,?4)",
                        params![
                            key_name(key.kind),
                            key.value,
                            owned.path.as_str(),
                            sql::integer(fact.byte_start)?
                        ],
                    )
                    .map_err(sql::sql_error)?;
                    stats.fact_rows += 1;
                }
            }
        }
        for entry in &self.registry {
            let row = load_record(c, &entry.record_id, stats)?
                .ok_or_else(|| invalid("orphan registry owner"))?;
            let expected = super::link_facts::registry_keys(&RegistryEntry {
                id: entry.record_id.clone(),
                path: row.path.clone(),
                kind: row.record.kind(),
                aliases: super::scan::list(&row.record, "aliases"),
            })?;
            if entry.path != row.path || entry.keys != expected {
                return Err(invalid("registry keys differ from adopted record"));
            }
            admit_old(
                c,
                "SELECT kind,value,record_id,path FROM registry_match_keys INDEXED BY registry_match_owners WHERE record_id=?1",
                entry.record_id.as_str(),
                4,
                stats,
            )?;
            c.execute(
                "DELETE FROM registry_match_keys WHERE record_id=?1",
                [entry.record_id.as_str()],
            )
            .map_err(sql::sql_error)?;
            for key in &entry.keys {
                c.execute(
                    "INSERT INTO registry_match_keys VALUES(?1,?2,?3,?4)",
                    params![
                        key_name(key.kind),
                        key.value,
                        entry.record_id.as_str(),
                        entry.path.as_str()
                    ],
                )
                .map_err(sql::sql_error)?;
                stats.fact_rows += 1;
            }
        }
        for row in &delta.records {
            require_fact(c, row.record.id())?;
            let own:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM record_direct_paths p JOIN dependencies d ON d.path=p.path WHERE p.owner_id=?1 AND p.path=?2 AND d.expected_hash=?3)",params![row.record.id().as_str(),row.path.as_str(),row.hash.as_str()],|r|r.get(0)).map_err(sql::sql_error)?;
            if !own {
                return Err(invalid("changed normalized record lacks exact own state"));
            }
            let registered:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM registry_match_keys WHERE kind='id' AND value=?1 AND record_id=?1 AND path=?2)",params![row.record.id().as_str(),row.path.as_str()],|r|r.get(0)).map_err(sql::sql_error)?;
            if !registered {
                return Err(invalid(
                    "changed normalized record lacks its registry identity",
                ));
            }
        }
        Ok(())
    }
}
fn require_fact(c: &Connection, id: &RecordId) -> Result<()> {
    let present:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM record_eligibility_facts f JOIN records r ON r.id=f.record_id WHERE f.record_id=?1)",[id.as_str()],|r|r.get(0)).map_err(sql::sql_error)?;
    if present {
        Ok(())
    } else {
        Err(invalid("normalized fact or adopted owner missing"))
    }
}
fn load_record(c: &Connection, id: &RecordId, stats: &mut DeltaStats) -> Result<Option<RecordRow>> {
    let mut statement = c
        .prepare("SELECT id,kind,path,hash,row_json FROM records WHERE id=?1")
        .map_err(sql::sql_error)?;
    let mut rows = statement.query([id.as_str()]).map_err(sql::sql_error)?;
    let Some(row) = rows.next().map_err(sql::sql_error)? else {
        return Ok(None);
    };
    let mut bytes = 0usize;
    for column in 0..5 {
        let value = match row.get_ref(column).map_err(sql::sql_error)? {
            ValueRef::Text(bytes) => bytes,
            _ => return Err(invalid("record fields are not text")),
        };
        bytes = bytes.checked_add(value.len()).ok_or_else(budget)?;
    }
    charge_old(bytes, stats)?;
    let text = |column| -> Result<&str> {
        row.get_ref(column)
            .map_err(sql::sql_error)?
            .as_str()
            .map_err(|_| invalid("record field is not UTF-8"))
    };
    let decoded: RecordRow =
        serde_json::from_str(text(4)?).map_err(|_| invalid("record JSON is malformed"))?;
    if decoded.record.id() != id
        || text(0)? != id.as_str()
        || text(1)? != decoded.record.kind().as_str()
        || text(2)? != decoded.path.as_str()
        || text(3)? != decoded.hash.as_str()
    {
        return Err(invalid(
            "selected record JSON differs from normalized identity columns",
        ));
    }
    Ok(Some(decoded))
}

fn charge_old(bytes: usize, stats: &mut DeltaStats) -> Result<()> {
    if bytes > MAX_ROW_BYTES {
        return Err(budget());
    }
    stats.old_fact_bytes = stats
        .old_fact_bytes
        .checked_add(bytes)
        .filter(|n| {
            n.checked_add(stats.old_fts_bytes)
                .is_some_and(|sum| sum <= MAX_DELTA_BYTES)
        })
        .ok_or_else(budget)?;
    stats.old_rows = stats
        .old_rows
        .checked_add(1)
        .filter(|n| *n <= MAX_ROWS)
        .ok_or_else(budget)?;
    Ok(())
}
fn admit_old(
    c: &Connection,
    sql_text: &str,
    key: &str,
    columns: usize,
    stats: &mut DeltaStats,
) -> Result<()> {
    let mut statement = c.prepare(sql_text).map_err(sql::sql_error)?;
    let mut rows = statement.query([key]).map_err(sql::sql_error)?;
    while let Some(row) = rows.next().map_err(sql::sql_error)? {
        let mut bytes = 0usize;
        for column in 0..columns {
            let length = match row.get_ref(column).map_err(sql::sql_error)? {
                ValueRef::Null => 0,
                ValueRef::Integer(_) | ValueRef::Real(_) => 8,
                ValueRef::Text(b) | ValueRef::Blob(b) => b.len(),
            };
            bytes = bytes.checked_add(length).ok_or_else(budget)?;
        }
        charge_old(bytes, stats)?;
    }
    Ok(())
}

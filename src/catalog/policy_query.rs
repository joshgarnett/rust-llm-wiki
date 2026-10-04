//! Selected policy reads. Results are complete or fail the shared read budget.
use super::{
    policy_facts::{PolicyKind, PolicyRow, PolicyState},
    query::QuerySnapshot,
    query_types::QueryCatalog,
    sql,
};
use crate::{
    domain::{Blake3Hash, ErrorCode, RecordId, Result, VaultRelativePath, WikiError},
    graph::policy_inputs::PolicyInputKey,
};
use rusqlite::params_from_iter;
use std::collections::{BTreeMap, BTreeSet};

fn corrupt() -> WikiError {
    WikiError::new(
        ErrorCode::IndexCorrupt,
        "normalized policy rows differ from selected scope",
    )
}
fn rebuild() -> WikiError {
    let mut e = WikiError::new(
        ErrorCode::OfflineUnavailable,
        "normalized receipt policy layout unavailable; run index rebuild",
    );
    e.hint = Some("run index rebuild".into());
    e
}
fn bounded(n: usize) -> Result<()> {
    if n > 4096 {
        Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "policy input count exceeds 4096",
        ))
    } else {
        Ok(())
    }
}

impl QuerySnapshot {
    fn policy_select(&self, query: &str, inputs: Vec<String>) -> Result<Vec<PolicyRow>> {
        for input in &inputs {
            self.reserve_fact_input(input.len())?;
        }
        let mut statement = self.connection().prepare(query).map_err(sql::sql_error)?;
        let mut cursor = statement
            .query(params_from_iter(inputs.iter()))
            .map_err(sql::sql_error)?;
        let mut result = Vec::new();
        while let Some(row) = cursor.next().map_err(sql::sql_error)? {
            self.reserve_refresh_row(row, 4)?;
            let columns = [row.get(0), row.get(1), row.get(2), row.get(3)]
                .into_iter()
                .collect::<std::result::Result<Vec<String>, _>>()
                .map_err(|_| corrupt())?;
            result.push(PolicyRow::from_columns(
                columns.try_into().map_err(|_| corrupt())?,
            )?);
        }
        Ok(result)
    }
    pub(crate) fn require_policy_layout(&self) -> Result<()> {
        if self.policy_layout_verified.get() {
            return Ok(());
        }
        self.require_fact_layout()?;
        // sqlite_schema lookup is bounded by name and never inspects policy data.
        let mut metadata = self.connection().prepare("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='policy_facts')").map_err(sql::sql_error)?;
        let mut cursor = metadata.query([]).map_err(sql::sql_error)?;
        let row = cursor.next().map_err(sql::sql_error)?.ok_or_else(corrupt)?;
        self.reserve_refresh_row(row, 1)?;
        let table: bool = row.get(0).map_err(|_| corrupt())?;
        drop(cursor);
        drop(metadata);
        if !table {
            return Err(rebuild());
        }
        for (index, expected) in [
            (
                "sqlite_autoindex_policy_facts_1",
                ["family", "key", "owner"],
            ),
            ("policy_facts_owner", ["family", "owner", "key"]),
        ] {
            self.reserve_fact_input(index.len())?;
            let mut statement = self
                .connection()
                .prepare("SELECT name FROM pragma_index_info(?1) ORDER BY seqno")
                .map_err(sql::sql_error)?;
            let mut cursor = statement.query([index]).map_err(sql::sql_error)?;
            let mut names = Vec::new();
            while let Some(row) = cursor.next().map_err(sql::sql_error)? {
                self.reserve_refresh_row(row, 1)?;
                names.push(row.get::<_, String>(0).map_err(|_| corrupt())?);
            }
            if names != expected {
                return Err(corrupt());
            }
        }
        // Preparing forced indexed queries is also a preflight for the exact DDL.
        self.connection().prepare("SELECT family,key,owner,value FROM policy_facts INDEXED BY policy_facts_owner WHERE family=?1 AND owner=?2 ORDER BY key").map_err(|_| corrupt())?;
        let rows = self.policy_select("SELECT family,key,owner,value FROM policy_facts INDEXED BY sqlite_autoindex_policy_facts_1 WHERE family='layout' ORDER BY key,owner", vec![])?;
        match rows.as_slice() {
            [PolicyRow::Layout(1)] => {
                self.policy_layout_verified.set(true);
                Ok(())
            }
            [] => Err(rebuild()),
            _ => Err(corrupt()),
        }
    }
    pub(crate) fn policy_members(
        &self,
        key: &PolicyInputKey,
        excluded_paths: &BTreeSet<VaultRelativePath>,
    ) -> Result<Vec<(VaultRelativePath, Blake3Hash)>> {
        self.require_policy_layout()?;
        if excluded_paths.len() > 16 {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "policy exclusions exceed 16",
            ));
        }
        let encoded = sql::json(key)?;
        let mut inputs = vec![encoded.clone()];
        inputs.extend(excluded_paths.iter().map(ToString::to_string));
        let exclusions = if excluded_paths.is_empty() {
            String::new()
        } else {
            format!(
                " AND owner NOT IN ({})",
                (0..excluded_paths.len())
                    .map(|_| "?")
                    .collect::<Vec<_>>()
                    .join(",")
            )
        };
        self.policy_select(&format!("SELECT family,key,owner,value FROM policy_facts INDEXED BY sqlite_autoindex_policy_facts_1 WHERE family='membership' AND key=?{exclusions} ORDER BY owner"), inputs)?.into_iter().map(|r| match r { PolicyRow::Membership {key: actual, path, hash} if actual == *key && !excluded_paths.contains(&path) => Ok((path,hash)), _ => Err(corrupt()) }).collect()
    }
    pub(crate) fn policy_affected(
        &self,
        keys: &BTreeSet<PolicyInputKey>,
        paths: &BTreeSet<VaultRelativePath>,
    ) -> Result<BTreeSet<PolicyKind>> {
        bounded(keys.len().saturating_add(paths.len()))?;
        self.require_policy_layout()?;
        let mut result = BTreeSet::new();
        for key in keys {
            for row in self.policy_select("SELECT family,key,owner,value FROM policy_facts INDEXED BY sqlite_autoindex_policy_facts_1 WHERE family='dependency' AND key=? ORDER BY owner", vec![sql::json(key)?])? { match row { PolicyRow::Dependency {kind,key: actual} if actual == *key => {result.insert(kind);}, _ => return Err(corrupt()) } }
        }
        for path in paths {
            for row in self.policy_select("SELECT family,key,owner,value FROM policy_facts INDEXED BY sqlite_autoindex_policy_facts_1 WHERE family='read_path' AND key=? ORDER BY owner", vec![path.to_string()])? { match row { PolicyRow::ReadPath {kind,path: actual,..} if actual == *path => {result.insert(kind);}, _ => return Err(corrupt()) } }
        }
        Ok(result)
    }
    pub(crate) fn policy_state(&self, kind: PolicyKind) -> Result<PolicyState> {
        self.require_policy_layout()?;
        let rows = self.policy_select("SELECT family,key,owner,value FROM policy_facts INDEXED BY sqlite_autoindex_policy_facts_1 WHERE family='state' AND key=? ORDER BY owner", vec![kind.name().into()])?;
        match rows.as_slice() {
            [
                PolicyRow::State {
                    kind: actual,
                    state,
                },
            ] if *actual == kind => Ok(state.clone()),
            _ => Err(corrupt()),
        }
    }
    /// Complete old materialized outputs, only when a global policy result is
    /// actually being replaced. Retractions must discover disconnected owners
    /// as well as the newly emitted outputs; all rows share the reader budget.
    pub(crate) fn policy_output_rows(&self, kind: PolicyKind) -> Result<Vec<PolicyRow>> {
        self.policy_state(kind)?;
        let families: &[&str] = match kind {
            PolicyKind::Remap => &["alias", "family_member", "remap_edge"],
            PolicyKind::Review => &["review_edge"],
        };
        let mut result = Vec::new();
        for family in families {
            result.extend(self.policy_select(
                "SELECT family,key,owner,value FROM policy_facts INDEXED BY sqlite_autoindex_policy_facts_1 WHERE family=? ORDER BY key,owner",
                vec![(*family).into()],
            )?);
        }
        Ok(result)
    }
    pub(crate) fn policy_alias_authority(
        &self,
        ids: &BTreeSet<RecordId>,
    ) -> Result<BTreeSet<RecordId>> {
        bounded(ids.len())?;
        self.require_policy_layout()?;
        let mut result = BTreeSet::new();
        for id in ids {
            for row in self.policy_select("SELECT family,key,owner,value FROM policy_facts INDEXED BY sqlite_autoindex_policy_facts_1 WHERE family='alias' AND key=? ORDER BY owner", vec![id.to_string()])? { match row { PolicyRow::Alias(actual) if actual == *id => {result.insert(actual);}, _ => return Err(corrupt()) } }
        }
        Ok(result)
    }
    pub(crate) fn policy_family_members(
        &self,
        ids: &BTreeSet<RecordId>,
    ) -> Result<BTreeMap<Blake3Hash, BTreeSet<RecordId>>> {
        bounded(ids.len())?;
        self.require_policy_layout()?;
        let mut selected = BTreeSet::new();
        for id in ids {
            for row in self.policy_select("SELECT family,key,owner,value FROM policy_facts INDEXED BY sqlite_autoindex_policy_facts_1 WHERE family='family_member' AND key=? ORDER BY owner", vec![id.to_string()])? { match row { PolicyRow::FamilyMember {family,id: actual} if actual == *id => {selected.insert(family);}, _ => return Err(corrupt()) } }
        }
        let mut result = BTreeMap::new();
        for selected_family in selected {
            let mut members = BTreeSet::new();
            for row in self.policy_select("SELECT family,key,owner,value FROM policy_facts INDEXED BY policy_facts_owner WHERE family='family_member' AND owner=? ORDER BY key", vec![selected_family.to_string()])? { match row { PolicyRow::FamilyMember {family,id} if family == selected_family => {members.insert(id);}, _ => return Err(corrupt()) } }
            if members.is_empty()
                || Blake3Hash::digest(sql::json(&members)?.as_bytes()) != selected_family
            {
                return Err(corrupt());
            }
            result.insert(selected_family, members);
        }
        Ok(result)
    }
    pub(crate) fn policy_edges(
        &self,
        kind: PolicyKind,
        id: &RecordId,
        reverse: bool,
    ) -> Result<Vec<(RecordId, RecordId)>> {
        self.require_policy_layout()?;
        let family = match kind {
            PolicyKind::Remap => "remap_edge",
            PolicyKind::Review => "review_edge",
        };
        let query = if reverse {
            "SELECT family,key,owner,value FROM policy_facts INDEXED BY policy_facts_owner WHERE family=? AND owner=? ORDER BY key"
        } else {
            "SELECT family,key,owner,value FROM policy_facts INDEXED BY sqlite_autoindex_policy_facts_1 WHERE family=? AND key=? ORDER BY owner"
        };
        self.policy_select(query, vec![family.into(), id.to_string()])?
            .into_iter()
            .map(|row| match row {
                PolicyRow::Edge {
                    kind: actual,
                    before,
                    after,
                } if actual == kind && if reverse { after == *id } else { before == *id } => {
                    Ok((before, after))
                }
                _ => Err(corrupt()),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    #[test]
    fn selected_access_plans_use_both_policy_indexes() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE policy_facts(family TEXT NOT NULL,key TEXT NOT NULL,owner TEXT NOT NULL,value TEXT NOT NULL,PRIMARY KEY(family,key,owner)); CREATE INDEX policy_facts_owner ON policy_facts(family,owner,key);").unwrap();
        for query in [
            "SELECT family,key,owner,value FROM policy_facts INDEXED BY sqlite_autoindex_policy_facts_1 WHERE family=?1 AND key=?2 ORDER BY owner",
            "SELECT family,key,owner,value FROM policy_facts INDEXED BY policy_facts_owner WHERE family=?1 AND owner=?2 ORDER BY key",
        ] {
            let details: Vec<String> = db
                .prepare(&format!("EXPLAIN QUERY PLAN {query}"))
                .unwrap()
                .query_map(["membership", "scope"], |r| r.get(3))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            assert!(
                details
                    .iter()
                    .any(|d| d.contains("SEARCH policy_facts USING INDEX")),
                "{details:?}"
            );
            assert!(
                !details
                    .iter()
                    .any(|d| d.contains("SCAN") || d.contains("TEMP B-TREE")),
                "{details:?}"
            );
        }
    }
    #[test]
    fn sixteen_exclusions_are_bound_and_applied_before_iteration() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE policy_facts(family TEXT NOT NULL,key TEXT NOT NULL,owner TEXT NOT NULL,value TEXT NOT NULL,PRIMARY KEY(family,key,owner));").unwrap();
        for n in 0..18 {
            db.execute(
                "INSERT INTO policy_facts VALUES('membership','scope',?1,'hash')",
                [format!("{n:02}.md")],
            )
            .unwrap();
        }
        let exclusions = (0..16).map(|_| "?").collect::<Vec<_>>().join(",");
        let query = format!(
            "SELECT owner FROM policy_facts INDEXED BY sqlite_autoindex_policy_facts_1 WHERE family='membership' AND key=? AND owner NOT IN ({exclusions}) ORDER BY owner"
        );
        let mut binds = vec!["scope".to_owned()];
        binds.extend((0..16).map(|n| format!("{n:02}.md")));
        let mut statement = db.prepare(&query).unwrap();
        assert_eq!(statement.parameter_count(), 17);
        let rows: Vec<String> = statement
            .query_map(params_from_iter(&binds), |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows, ["16.md", "17.md"]);
    }
}

//! Bounded policy relation replacement in the caller's publication transaction.
use super::{
    normalized_delta::{
        CatalogDelta, DeltaStats, DocumentMutation, MAX_DELTA_BYTES, MAX_ROW_BYTES, MAX_ROWS,
        counted,
    },
    normalized_fact_delta::charge_old,
    policy_facts::{POLICY_LAYOUT, PolicyKind, PolicyRow, PolicyState},
    sql,
};
use crate::{
    domain::{Blake3Hash, ErrorCode, Result, VaultRelativePath, WikiError},
    graph::policy_inputs::{PolicyInputKey, policy_membership_keys},
};
use rusqlite::{Connection, params, types::ValueRef};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PolicyDelta {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retired_owners: Vec<VaultRelativePath>,
    pub memberships: Vec<PolicyMembershipUpdate>,
    pub replacements: Vec<PolicyReplacement>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PolicyMembershipUpdate {
    pub path: VaultRelativePath,
    pub hash: Blake3Hash,
    pub keys: BTreeSet<PolicyInputKey>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PolicyReplacement {
    pub kind: PolicyKind,
    pub rows: Vec<PolicyRow>,
}
fn bad(message: &str) -> WikiError {
    WikiError::new(ErrorCode::IndexCorrupt, message)
}
fn budget() -> WikiError {
    WikiError::new(
        ErrorCode::BudgetExceeded,
        "policy delta exceeds cumulative allowance",
    )
}
fn admit<T: Serialize>(row: &T, count: &mut usize) -> Result<()> {
    *count = count
        .checked_add(1)
        .filter(|n| *n <= MAX_ROWS)
        .ok_or_else(budget)?;
    counted(row, MAX_ROW_BYTES)?;
    Ok(())
}
fn family_groups(rows: &[PolicyRow]) -> Result<()> {
    let mut groups = BTreeMap::new();
    for row in rows {
        if let PolicyRow::FamilyMember { family, id } = row {
            groups
                .entry(family)
                .or_insert_with(BTreeSet::new)
                .insert(id);
        }
    }
    for (family, ids) in groups {
        if ids.is_empty() || Blake3Hash::digest(sql::json(&ids)?.as_bytes()) != *family {
            return Err(bad("policy family hash differs from exact members"));
        }
    }
    Ok(())
}
impl PolicyDelta {
    pub(super) fn validate(&self, delta: &CatalogDelta, count: &mut usize) -> Result<()> {
        counted(self, MAX_DELTA_BYTES)?;
        let moves: Vec<_> = delta
            .documents
            .iter()
            .filter_map(|d| {
                if let DocumentMutation::MovePage { from, .. } = d {
                    Some(from)
                } else {
                    None
                }
            })
            .collect();
        if self.retired_owners.iter().collect::<Vec<_>>() != moves {
            return Err(bad("policy retired owners differ from sealed Page move"));
        }
        for retired in &self.retired_owners {
            admit(retired, count)?;
        }
        let mut owners = BTreeSet::new();
        for update in &self.memberships {
            if !owners.insert(&update.path) {
                return Err(bad("duplicate policy membership owner"));
            }
            admit(update, count)?; // An empty owner clear still consumes bounded work.
            let record = delta
                .records
                .iter()
                .find(|r| r.path == update.path && r.hash == update.hash);
            let document = delta
                .documents
                .iter()
                .find_map(|d| match d {
                    DocumentMutation::Put { row } | DocumentMutation::MovePage { row, .. }
                        if row.path == update.path =>
                    {
                        Some(row)
                    }
                    _ => None,
                })
                .ok_or_else(|| bad("policy membership lacks exact document replacement"))?;
            let note = crate::records::parse_note(document.raw_text.as_bytes());
            if document.hash != update.hash
                || document.owner_revision.is_some()
                || document.source_id.is_some()
                || Blake3Hash::digest(document.raw_text.as_bytes()) != update.hash
                || policy_membership_keys(&update.path, &note)? != update.keys
            {
                return Err(bad(
                    "policy membership differs from document bytes or classification",
                ));
            }
            match record {
                Some(record)
                    if document.record_id.as_ref() == Some(record.record.id())
                        && document.kind == Some(record.record.kind())
                        && note.canonical.as_ref() == Some(&record.record) => {}
                None if !moves.is_empty()
                    && document.record_id.is_none()
                    && *document
                        == super::row_projection::canonical_document(&update.path, &note, None) =>
                {
                    let claims = delta
                        .claims
                        .iter()
                        .find(|owned| owned.path == update.path)
                        .ok_or_else(|| bad("unadopted rewrite lacks complete identity claims"))?;
                    let expected: Vec<_> = crate::sources::identity::readable_ids(&note)
                        .into_iter()
                        .map(|id| super::types::IdentityClaimRow {
                            id,
                            path: update.path.clone(),
                            hash: note.source_hash.clone(),
                            kind: note.canonical.as_ref().map(|record| record.kind()),
                        })
                        .collect();
                    if claims.rows != expected {
                        return Err(bad(
                            "unadopted rewrite identity claims differ from exact parsed bytes",
                        ));
                    }
                }
                _ => return Err(bad("policy membership changes adopted document identity")),
            }
            for key in &update.keys {
                admit(
                    &PolicyRow::Membership {
                        key: key.clone(),
                        path: update.path.clone(),
                        hash: update.hash.clone(),
                    }
                    .columns()?,
                    count,
                )?;
            }
        }
        let mut kinds = BTreeSet::new();
        for replacement in &self.replacements {
            if !kinds.insert(replacement.kind) {
                return Err(bad("duplicate policy replacement kind"));
            }
            admit(&replacement.kind, count)?;
            let mut keys = BTreeSet::new();
            let mut state = None;
            let mut successful = false;
            for row in &replacement.rows {
                match row {
                    PolicyRow::State { kind, state: value } if *kind == replacement.kind => {
                        state = Some(value);
                    }
                    PolicyRow::Dependency { kind, .. } | PolicyRow::ReadPath { kind, .. }
                        if *kind == replacement.kind => {}
                    PolicyRow::Edge { kind, .. } if *kind == replacement.kind => {
                        successful = true;
                    }
                    PolicyRow::Alias(_) | PolicyRow::FamilyMember { .. }
                        if replacement.kind == PolicyKind::Remap =>
                    {
                        successful = true;
                    }
                    _ => return Err(bad("cross-owned policy replacement row")),
                }
                let columns = row.columns()?;
                if PolicyRow::from_columns(columns.clone())? != *row
                    || !keys.insert((columns[0].clone(), columns[1].clone(), columns[2].clone()))
                {
                    return Err(bad("duplicate or noncanonical policy replacement row"));
                }
                admit(&columns, count)?;
            }
            match state {
                None => return Err(bad("policy replacement lacks mandatory state")),
                Some(PolicyState::Verified) => {}
                Some(_) if successful => {
                    return Err(bad(
                        "unsuccessful policy state carries successful result rows",
                    ));
                }
                Some(_) => {}
            }
            family_groups(&replacement.rows)?;
        }
        Ok(())
    }

    pub(super) fn apply(&self, c: &Connection, stats: &mut DeltaStats) -> Result<()> {
        let layout = scan(c, "family=?1", "layout", stats)?;
        if layout.is_empty() {
            return Err(WikiError::new(
                ErrorCode::OfflineUnavailable,
                "normalized policy layout absent; explicitly rebuild the catalog",
            ));
        }
        if layout != vec![PolicyRow::Layout(POLICY_LAYOUT)] {
            return Err(bad("foreign normalized policy layout"));
        }
        // Admit every old row before the first mutation, including complete remap fanout.
        let mut predicates: Vec<(&str, String)> = Vec::new();
        for update in &self.memberships {
            predicates.push(("family='membership' AND owner=?1", update.path.to_string()));
        }
        for retired in &self.retired_owners {
            predicates.push(("family='membership' AND owner=?1", retired.to_string()));
        }
        for replacement in &self.replacements {
            let kind = replacement.kind.name().to_owned();
            predicates.push(("family='state' AND key=?1", kind.clone()));
            predicates.push(("family='dependency' AND owner=?1", kind.clone()));
            predicates.push(("family='read_path' AND owner=?1", kind));
            predicates.push((
                "family=?1",
                if replacement.kind == PolicyKind::Remap {
                    "remap_edge"
                } else {
                    "review_edge"
                }
                .into(),
            ));
            if replacement.kind == PolicyKind::Remap {
                predicates.push(("family=?1", "alias".into()));
                predicates.push(("family=?1", "family_member".into()));
            }
        }
        for (predicate, parameter) in &predicates {
            let rows = scan(c, predicate, parameter, stats)?;
            family_groups(&rows)?;
        }
        for (predicate, parameter) in predicates {
            c.execute(
                &format!("DELETE FROM policy_facts WHERE {predicate}"),
                [parameter],
            )
            .map_err(sql::sql_error)?;
        }
        for update in &self.memberships {
            for key in &update.keys {
                insert(
                    c,
                    &PolicyRow::Membership {
                        key: key.clone(),
                        path: update.path.clone(),
                        hash: update.hash.clone(),
                    },
                    stats,
                )?;
            }
        }
        for replacement in &self.replacements {
            for row in &replacement.rows {
                insert(c, row, stats)?;
            }
        }
        Ok(())
    }
}
fn insert(c: &Connection, row: &PolicyRow, stats: &mut DeltaStats) -> Result<()> {
    let [family, key, owner, value] = row.columns()?;
    c.execute(
        "INSERT INTO policy_facts VALUES(?1,?2,?3,?4)",
        params![family, key, owner, value],
    )
    .map_err(sql::sql_error)?;
    stats.fact_rows += 1;
    Ok(())
}
fn scan(
    c: &Connection,
    predicate: &str,
    parameter: &str,
    stats: &mut DeltaStats,
) -> Result<Vec<PolicyRow>> {
    let index = if predicate.contains("owner=?1") {
        " INDEXED BY policy_facts_owner"
    } else {
        ""
    };
    let mut statement = c
        .prepare(&format!(
            "SELECT family,key,owner,value FROM policy_facts{index} WHERE {predicate}"
        ))
        .map_err(sql::sql_error)?;
    let mut cursor = statement.query([parameter]).map_err(sql::sql_error)?;
    let mut result = Vec::new();
    while let Some(row) = cursor.next().map_err(sql::sql_error)? {
        let mut bytes = 0usize;
        for column in 0..4 {
            let ValueRef::Text(value) = row.get_ref(column).map_err(sql::sql_error)? else {
                return Err(bad("nontext normalized policy column"));
            };
            bytes = bytes.checked_add(value.len()).ok_or_else(budget)?;
        }
        charge_old(bytes, stats)?;
        let columns = [row.get(0), row.get(1), row.get(2), row.get(3)]
            .into_iter()
            .collect::<rusqlite::Result<Vec<String>>>()
            .map_err(sql::sql_error)?;
        result.push(PolicyRow::from_columns(
            columns.try_into().map_err(|_| bad("policy column count"))?,
        )?);
    }
    Ok(result)
}

impl PolicyReplacement {
    pub(crate) fn from_remap(
        result: &Result<Option<crate::graph::remap::VerifiedDecisionPolicy>>,
        trace: crate::graph::policy_inputs::PolicyInputTrace,
    ) -> Result<Self> {
        let mut replacement = Self::from_trace(PolicyKind::Remap, result, trace)?;
        if let Ok(Some(policy)) = result {
            for id in policy.policy_aliases() {
                replacement.rows.push(PolicyRow::Alias(id.clone()));
            }
            for members in policy.policy_families().iter().collect::<BTreeSet<_>>() {
                let family = Blake3Hash::digest(sql::json(members)?.as_bytes());
                for id in members {
                    replacement.rows.push(PolicyRow::FamilyMember {
                        family: family.clone(),
                        id: id.clone(),
                    });
                }
            }
            for (before, after) in policy.supersession_edges() {
                replacement.rows.push(PolicyRow::Edge {
                    kind: PolicyKind::Remap,
                    before: before.clone(),
                    after: after.clone(),
                });
            }
        }
        Ok(replacement)
    }
    pub(crate) fn from_review(
        result: &Result<Option<crate::graph::review::VerifiedReviewPolicy>>,
        trace: crate::graph::policy_inputs::PolicyInputTrace,
    ) -> Result<Self> {
        let mut replacement = Self::from_trace(PolicyKind::Review, result, trace)?;
        if let Ok(Some(policy)) = result {
            for (before, after) in policy.supersession_edges() {
                replacement.rows.push(PolicyRow::Edge {
                    kind: PolicyKind::Review,
                    before: before.clone(),
                    after: after.clone(),
                });
            }
        }
        Ok(replacement)
    }
    fn from_trace<T>(
        kind: PolicyKind,
        result: &Result<Option<T>>,
        trace: crate::graph::policy_inputs::PolicyInputTrace,
    ) -> Result<Self> {
        if let Err(error) = result {
            if error.code == ErrorCode::BudgetExceeded {
                return Err(error.clone());
            }
        }
        let mut rows = vec![PolicyRow::State {
            kind,
            state: PolicyState::of(result)?,
        }];
        for key in trace.keys {
            rows.push(PolicyRow::Dependency { kind, key });
        }
        for (path, hash) in trace.paths {
            rows.push(PolicyRow::ReadPath { kind, path, hash });
        }
        Ok(Self { kind, rows })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_compatible_families_can_overlap() {
        let a = crate::domain::RecordId::new("decision_a").unwrap();
        let b = crate::domain::RecordId::new("decision_b").unwrap();
        let c = crate::domain::RecordId::new("decision_c").unwrap();
        let mut rows = Vec::new();
        for ids in [
            BTreeSet::from([a.clone(), b.clone()]),
            BTreeSet::from([b, c]),
        ] {
            let family = Blake3Hash::digest(sql::json(&ids).unwrap().as_bytes());
            rows.extend(ids.into_iter().map(|id| PolicyRow::FamilyMember {
                family: family.clone(),
                id,
            }));
        }
        family_groups(&rows).unwrap();
    }
    fn database() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE policy_facts(family TEXT,key TEXT,owner TEXT,value TEXT,PRIMARY KEY(family,key,owner)); CREATE INDEX policy_facts_owner ON policy_facts(family,owner,key);").unwrap();
        c
    }
    #[test]
    fn absent_layout_requires_explicit_rebuild() {
        let c = database();
        let delta = PolicyDelta {
            retired_owners: vec![],
            memberships: vec![],
            replacements: vec![],
        };
        assert_eq!(
            delta
                .apply(&c, &mut DeltaStats::default())
                .unwrap_err()
                .code,
            ErrorCode::OfflineUnavailable
        );
    }
    #[test]
    fn old_row_budget_failure_precedes_all_deletes() {
        let c = database();
        let mut stats = DeltaStats::default();
        insert(&c, &PolicyRow::Layout(1), &mut stats).unwrap();
        insert(
            &c,
            &PolicyRow::State {
                kind: PolicyKind::Review,
                state: PolicyState::Verified,
            },
            &mut stats,
        )
        .unwrap();
        let replacement = PolicyReplacement::from_review(&Ok(None), Default::default()).unwrap();
        let delta = PolicyDelta {
            retired_owners: vec![],
            memberships: vec![],
            replacements: vec![replacement],
        };
        stats.old_rows = MAX_ROWS - 1;
        assert_eq!(
            delta.apply(&c, &mut stats).unwrap_err().code,
            ErrorCode::BudgetExceeded
        );
        let value: String = c
            .query_row(
                "SELECT value FROM policy_facts WHERE family='state' AND key='review'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(value, sql::json(&PolicyState::Verified).unwrap());
    }
    #[test]
    fn replacing_review_retains_remap_and_membership() {
        let c = database();
        let mut stats = DeltaStats::default();
        insert(&c, &PolicyRow::Layout(1), &mut stats).unwrap();
        insert(
            &c,
            &PolicyRow::State {
                kind: PolicyKind::Remap,
                state: PolicyState::Verified,
            },
            &mut stats,
        )
        .unwrap();
        insert(
            &c,
            &PolicyRow::State {
                kind: PolicyKind::Review,
                state: PolicyState::Verified,
            },
            &mut stats,
        )
        .unwrap();
        let path = VaultRelativePath::new("policy.md").unwrap();
        insert(
            &c,
            &PolicyRow::Membership {
                key: PolicyInputKey::Extractions,
                path,
                hash: Blake3Hash::digest(b"x"),
            },
            &mut stats,
        )
        .unwrap();
        let delta = PolicyDelta {
            retired_owners: vec![],
            memberships: vec![],
            replacements: vec![
                PolicyReplacement::from_review(&Ok(None), Default::default()).unwrap(),
            ],
        };
        delta.apply(&c, &mut stats).unwrap();
        let n: i64 = c.query_row("SELECT count(*) FROM policy_facts WHERE family='membership' OR (family='state' AND key='remap')", [], |r|r.get(0)).unwrap();
        assert_eq!(n, 2);
    }
}

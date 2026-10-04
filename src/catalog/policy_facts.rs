//! Fixed normalized receipt-policy relations. One row represents one input,
//! witness or authority member; no row contains a vault or a complete family.
use crate::{
    domain::{Blake3Hash, RecordId, Result, VaultRelativePath, WikiError},
    graph::{
        policy_inputs::{PolicyInputKey, PolicyInputTrace, policy_membership_keys},
        remap::VerifiedDecisionPolicy,
        review::VerifiedReviewPolicy,
    },
    records::ParsedNote,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const POLICY_LAYOUT: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PolicyKind {
    Remap,
    Review,
}
impl PolicyKind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Remap => "remap",
            Self::Review => "review",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "status",
    content = "error",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(crate) enum PolicyState {
    Absent,
    Verified,
    Failed(serde_json::Value),
}
impl PolicyState {
    pub(super) fn of<T>(result: &Result<Option<T>>) -> Result<Self> {
        Ok(match result {
            Ok(None) => Self::Absent,
            Ok(Some(_)) => Self::Verified,
            Err(error) => Self::Failed(
                serde_json::to_value(error).map_err(|e| WikiError::invalid(e.to_string()))?,
            ),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PolicyMembership {
    pub hash: Blake3Hash,
    pub keys: BTreeSet<PolicyInputKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NormalizedPolicyFacts {
    pub version: u32,
    pub states: BTreeMap<PolicyKind, PolicyState>,
    pub traces: BTreeMap<PolicyKind, PolicyInputTrace>,
    pub memberships: BTreeMap<VaultRelativePath, PolicyMembership>,
    pub aliases: BTreeSet<RecordId>,
    pub families: BTreeSet<BTreeSet<RecordId>>,
    pub edges: BTreeMap<PolicyKind, BTreeSet<(RecordId, RecordId)>>,
}

impl NormalizedPolicyFacts {
    pub(crate) fn from_evaluation(
        notes: &BTreeMap<VaultRelativePath, ParsedNote>,
        remap: &Result<Option<VerifiedDecisionPolicy>>,
        review: &Result<Option<VerifiedReviewPolicy>>,
        remap_trace: PolicyInputTrace,
        review_trace: PolicyInputTrace,
    ) -> Result<Self> {
        let mut memberships = BTreeMap::new();
        for (path, note) in notes {
            let keys = policy_membership_keys(path, note)?;
            if !keys.is_empty() {
                memberships.insert(
                    path.clone(),
                    PolicyMembership {
                        hash: Blake3Hash::digest(&note.raw),
                        keys,
                    },
                );
            }
        }
        let decision = remap.as_ref().ok().and_then(Option::as_ref);
        Ok(Self {
            version: POLICY_LAYOUT,
            states: BTreeMap::from([
                (PolicyKind::Remap, PolicyState::of(remap)?),
                (PolicyKind::Review, PolicyState::of(review)?),
            ]),
            traces: BTreeMap::from([
                (PolicyKind::Remap, remap_trace),
                (PolicyKind::Review, review_trace),
            ]),
            memberships,
            aliases: decision
                .map(|p| p.policy_aliases().clone())
                .unwrap_or_default(),
            families: decision
                .map(|p| p.policy_families().iter().cloned().collect())
                .unwrap_or_default(),
            edges: BTreeMap::from([
                (
                    PolicyKind::Remap,
                    decision
                        .map(|p| p.supersession_edges().clone())
                        .unwrap_or_default(),
                ),
                (
                    PolicyKind::Review,
                    review
                        .as_ref()
                        .ok()
                        .and_then(Option::as_ref)
                        .map(|p| p.supersession_edges().clone())
                        .unwrap_or_default(),
                ),
            ]),
        })
    }

    pub(crate) fn visit(&self, visit: &mut dyn FnMut(PolicyRow) -> Result<()>) -> Result<()> {
        if self.version != POLICY_LAYOUT
            || self.states.len() != 2
            || self.traces.len() != 2
            || self.edges.len() != 2
        {
            return Err(WikiError::invalid(
                "receipt-policy fact layout is incomplete",
            ));
        }
        visit(PolicyRow::Layout(self.version))?;
        for kind in [PolicyKind::Remap, PolicyKind::Review] {
            let state = self
                .states
                .get(&kind)
                .ok_or_else(|| WikiError::invalid("policy result missing"))?;
            visit(PolicyRow::State {
                kind,
                state: state.clone(),
            })?;
            let trace = self
                .traces
                .get(&kind)
                .ok_or_else(|| WikiError::invalid("policy trace missing"))?;
            for key in &trace.keys {
                visit(PolicyRow::Dependency {
                    kind,
                    key: key.clone(),
                })?;
            }
            for (path, hash) in &trace.paths {
                visit(PolicyRow::ReadPath {
                    kind,
                    path: path.clone(),
                    hash: hash.clone(),
                })?;
            }
            for (before, after) in self
                .edges
                .get(&kind)
                .ok_or_else(|| WikiError::invalid("policy edges missing"))?
            {
                visit(PolicyRow::Edge {
                    kind,
                    before: before.clone(),
                    after: after.clone(),
                })?;
            }
        }
        for (path, member) in &self.memberships {
            for key in &member.keys {
                visit(PolicyRow::Membership {
                    key: key.clone(),
                    path: path.clone(),
                    hash: member.hash.clone(),
                })?;
            }
        }
        for id in &self.aliases {
            visit(PolicyRow::Alias(id.clone()))?;
        }
        for members in &self.families {
            let family = Blake3Hash::digest(super::sql::json(members)?.as_bytes());
            for id in members {
                visit(PolicyRow::FamilyMember {
                    family: family.clone(),
                    id: id.clone(),
                })?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "family",
    content = "row",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(crate) enum PolicyRow {
    Layout(u32),
    State {
        kind: PolicyKind,
        state: PolicyState,
    },
    Dependency {
        kind: PolicyKind,
        key: PolicyInputKey,
    },
    ReadPath {
        kind: PolicyKind,
        path: VaultRelativePath,
        hash: Blake3Hash,
    },
    Membership {
        key: PolicyInputKey,
        path: VaultRelativePath,
        hash: Blake3Hash,
    },
    Alias(RecordId),
    FamilyMember {
        family: Blake3Hash,
        id: RecordId,
    },
    Edge {
        kind: PolicyKind,
        before: RecordId,
        after: RecordId,
    },
}

impl PolicyRow {
    /// Encodings are closed, canonical and round-trip checked on reads. The SQL
    /// reverse index serves complete owner replacement and family membership.
    pub(crate) fn columns(&self) -> Result<[String; 4]> {
        let (family, key, owner, value) = match self {
            Self::Layout(version) => ("layout", String::new(), String::new(), version.to_string()),
            Self::State { kind, state } => (
                "state",
                kind.name().into(),
                String::new(),
                super::sql::json(state)?,
            ),
            Self::Dependency { kind, key } => (
                "dependency",
                super::sql::json(key)?,
                kind.name().into(),
                String::new(),
            ),
            Self::ReadPath { kind, path, hash } => (
                "read_path",
                path.to_string(),
                kind.name().into(),
                hash.to_string(),
            ),
            Self::Membership { key, path, hash } => (
                "membership",
                super::sql::json(key)?,
                path.to_string(),
                hash.to_string(),
            ),
            Self::Alias(id) => ("alias", id.to_string(), String::new(), String::new()),
            Self::FamilyMember { family, id } => (
                "family_member",
                id.to_string(),
                family.to_string(),
                String::new(),
            ),
            Self::Edge {
                kind,
                before,
                after,
            } => (
                match kind {
                    PolicyKind::Remap => "remap_edge",
                    PolicyKind::Review => "review_edge",
                },
                before.to_string(),
                after.to_string(),
                String::new(),
            ),
        };
        Ok([family.into(), key, owner, value])
    }
}

impl PolicyRow {
    pub(crate) fn from_columns(columns: [String; 4]) -> Result<Self> {
        use crate::domain::ErrorCode;
        let bad = || WikiError::new(ErrorCode::IndexCorrupt, "invalid normalized policy row");
        let [family, key, owner, value] = &columns;
        let kind = |s: &str| match s {
            "remap" => Ok(PolicyKind::Remap),
            "review" => Ok(PolicyKind::Review),
            _ => Err(bad()),
        };
        let id = |s: &str| RecordId::new(s).map_err(|_| bad());
        let path = |s: &str| VaultRelativePath::new(s).map_err(|_| bad());
        let hash = |s: &str| Blake3Hash::new(s).map_err(|_| bad());
        let input = |s: &str| serde_json::from_str::<PolicyInputKey>(s).map_err(|_| bad());
        let row = match family.as_str() {
            "layout" if key.is_empty() && owner.is_empty() && value == "1" => {
                Self::Layout(POLICY_LAYOUT)
            }
            "state" if owner.is_empty() => {
                let state: PolicyState = serde_json::from_str(value).map_err(|_| bad())?;
                if let PolicyState::Failed(error) = &state {
                    let decoded: WikiError =
                        serde_json::from_value(error.clone()).map_err(|_| bad())?;
                    if decoded.code == ErrorCode::BudgetExceeded
                        || serde_json::to_value(decoded).map_err(|_| bad())? != *error
                    {
                        return Err(bad());
                    }
                }
                Self::State {
                    kind: kind(key)?,
                    state,
                }
            }
            "dependency" if value.is_empty() => Self::Dependency {
                kind: kind(owner)?,
                key: input(key)?,
            },
            "read_path" => Self::ReadPath {
                kind: kind(owner)?,
                path: path(key)?,
                hash: hash(value)?,
            },
            "membership" => Self::Membership {
                key: input(key)?,
                path: path(owner)?,
                hash: hash(value)?,
            },
            "alias" if owner.is_empty() && value.is_empty() => Self::Alias(id(key)?),
            "family_member" if value.is_empty() => Self::FamilyMember {
                family: hash(owner)?,
                id: id(key)?,
            },
            "remap_edge" | "review_edge" if value.is_empty() => Self::Edge {
                kind: if family == "remap_edge" {
                    PolicyKind::Remap
                } else {
                    PolicyKind::Review
                },
                before: id(key)?,
                after: id(owner)?,
            },
            _ => return Err(bad()),
        };
        if row.columns().map_err(|_| bad())? != columns {
            return Err(bad());
        }
        Ok(row)
    }
}

#[cfg(test)]
mod codec_tests {
    use super::*;
    use crate::domain::ErrorCode;
    #[test]
    fn closed_rows_round_trip_and_reject_noncanonical_fields() {
        let id = RecordId::new("policy_codec").unwrap();
        let path = VaultRelativePath::new("policy.md").unwrap();
        let hash = Blake3Hash::digest(b"policy");
        let rows = [
            PolicyRow::Layout(1),
            PolicyRow::State {
                kind: PolicyKind::Remap,
                state: PolicyState::Absent,
            },
            PolicyRow::Dependency {
                kind: PolicyKind::Review,
                key: PolicyInputKey::CanonicalIdentity(id.clone()),
            },
            PolicyRow::ReadPath {
                kind: PolicyKind::Review,
                path: path.clone(),
                hash: hash.clone(),
            },
            PolicyRow::Membership {
                key: PolicyInputKey::Extractions,
                path,
                hash: hash.clone(),
            },
            PolicyRow::Alias(id.clone()),
            PolicyRow::FamilyMember {
                family: hash,
                id: id.clone(),
            },
            PolicyRow::Edge {
                kind: PolicyKind::Remap,
                before: id.clone(),
                after: id,
            },
        ];
        for row in rows {
            let cols = row.columns().unwrap();
            assert_eq!(PolicyRow::from_columns(cols.clone()).unwrap(), row);
            let mut malformed = cols;
            malformed[0] = "future".into();
            assert_eq!(
                PolicyRow::from_columns(malformed).unwrap_err().code,
                ErrorCode::IndexCorrupt
            );
        }
        for cols in [
            ["layout", "", "", "01"],
            ["layout", "", "", "2"],
            ["alias", "policy_codec", "extra", ""],
            [
                "state",
                "remap",
                "",
                "{\"status\":\"absent\",\"error\":null}",
            ],
            ["membership", "\"Extractions\"", "../outside.md", "bad"],
        ] {
            assert!(PolicyRow::from_columns(cols.map(str::to_owned)).is_err());
        }
    }
    #[test]
    fn persisted_failure_must_be_a_real_nonbudget_error() {
        for error in [
            serde_json::json!({"arbitrary":true}),
            serde_json::to_value(WikiError::new(ErrorCode::BudgetExceeded, "limited")).unwrap(),
        ] {
            let row = PolicyRow::State {
                kind: PolicyKind::Remap,
                state: PolicyState::Failed(error),
            };
            assert_eq!(
                PolicyRow::from_columns(row.columns().unwrap())
                    .unwrap_err()
                    .code,
                ErrorCode::IndexCorrupt
            );
        }
        let row = PolicyRow::State {
            kind: PolicyKind::Review,
            state: PolicyState::Failed(
                serde_json::to_value(WikiError::invalid("bad receipt")).unwrap(),
            ),
        };
        assert_eq!(
            PolicyRow::from_columns(row.columns().unwrap()).unwrap(),
            row
        );
    }
}

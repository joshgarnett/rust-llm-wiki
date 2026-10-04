//! Stream canonical rows against the selected catalog; scratch retains only row identities.
use super::{
    compact_audit::{CompactAudit, CompactStats},
    full_check_types::CheckBudget,
    link_facts::{MatchKey, OwnedLinkFact},
    normalized_build::counted_json,
    normalized_metadata::{self, MetadataRow, MetadataSink},
    sql,
    types::*,
};
use crate::{
    changes::ChangeEngine,
    domain::{Eligibility, ErrorCode, Result},
    vault::{ExpectedState, VaultFs, WriterPermit},
};
use rusqlite::{Connection, Params, params, types::ValueRef};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

const FAMILIES: [&str; 19] = [
    "documents",
    "graph_rows",
    "records",
    "identity_claims",
    "source_revision_identity",
    "source_evidence",
    "revision_tree_owners",
    "links",
    "link_facts",
    "link_match_keys",
    "registry_match_keys",
    "assertion_navigation_keys",
    "opposition_members",
    "record_eligibility_facts",
    "policy_facts",
    "record_direct_paths",
    "semantic_edges",
    "dependencies",
    "diagnostics",
];
fn text(value: &str) -> ValueRef<'_> {
    ValueRef::Text(value.as_bytes())
}
fn optional(value: Option<&str>) -> ValueRef<'_> {
    value.map(text).unwrap_or(ValueRef::Null)
}
fn eligibility(value: Eligibility) -> &'static str {
    match value {
        Eligibility::Current => "current",
        Eligibility::Historical => "historical",
        Eligibility::Invalid => "invalid",
        Eligibility::Stale => "stale",
        Eligibility::Withdrawn => "withdrawn",
        Eligibility::Unsupported => "unsupported",
    }
}
fn key_kind(key: &MatchKey) -> &'static str {
    use super::link_facts::MatchKeyKind::*;
    match key.kind {
        Id => "id",
        Path => "path",
        Basename => "basename",
        Alias => "alias",
    }
}

pub(crate) struct AuditSink<'a> {
    source: &'a Connection,
    scratch: &'a Connection,
    compact: CompactAudit<'a>,
    budget: CheckBudget,
    counts: BTreeMap<&'static str, u64>,
    checked_plans: BTreeSet<String>,
}
impl<'a> AuditSink<'a> {
    pub(crate) fn new(
        source: &'a Connection,
        scratch: &'a Connection,
        budget: CheckBudget,
    ) -> Result<Self> {
        let compact = CompactAudit::new(source, scratch, budget.clone())?;
        scratch.execute_batch("CREATE TABLE checked_rows(family TEXT NOT NULL,rid INTEGER NOT NULL,PRIMARY KEY(family,rid)) WITHOUT ROWID;")
            .map_err(|e| budget.sql_error(e))?;
        Ok(Self {
            source,
            scratch,
            compact,
            budget,
            counts: FAMILIES.into_iter().map(|s| (s, 0)).collect(),
            checked_plans: BTreeSet::new(),
        })
    }
    fn require_streaming_plan(&mut self, query: &str) -> Result<()> {
        if self.checked_plans.contains(query) {
            return Ok(());
        }
        self.budget.guard()?;
        let mut plan = self
            .source
            .prepare(&format!("EXPLAIN {query}"))
            .map_err(|e| self.budget.sql_error(e))?;
        // EXPLAIN does not evaluate parameters; null bindings suffice for this
        // fixed family of point lookups, prefix scans and table counts.
        let mut rows = plan.raw_query();
        while let Some(row) = rows.next().map_err(|e| self.budget.sql_error(e))? {
            self.budget.guard()?;
            let opcode: &str = row
                .get_ref(1)
                .map_err(|e| self.budget.sql_error(e))?
                .as_str()
                .map_err(|e| self.budget.fail(ErrorCode::IndexCorrupt, e.to_string()))?;
            if matches!(
                opcode,
                "SorterOpen"
                    | "SorterInsert"
                    | "SorterSort"
                    | "Sort"
                    | "OpenEphemeral"
                    | "OpenAutoindex"
            ) {
                return Err(self.budget.fail(
                    ErrorCode::CapabilityUnavailable,
                    "explicit check comparison requires unbounded temporary sorting",
                ));
            }
        }
        self.checked_plans.insert(query.to_owned());
        Ok(())
    }
    fn json<T: Serialize>(&self, value: &T) -> Result<String> {
        self.budget.guard()?;
        counted_json(value, self.budget.limits().max_row_bytes)
            .map_err(|e| self.budget.fail(e.code, e.message))?;
        sql::json(value).map_err(|e| self.budget.fail(e.code, e.message))
    }
    fn consumed(&self, family: &str, rid: i64) -> Result<bool> {
        self.scratch
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM checked_rows WHERE family=?1 AND rid=?2)",
                params![family, rid],
                |r| r.get(0),
            )
            .map_err(|e| self.budget.sql_error(e))
    }
    fn consume(&mut self, family: &'static str, rid: i64, repeat_owner: bool) -> Result<()> {
        let inserted = self
            .scratch
            .execute(
                "INSERT OR IGNORE INTO checked_rows(family,rid) VALUES(?1,?2)",
                params![family, rid],
            )
            .map_err(|e| self.budget.sql_error(e))?;
        if inserted == 0 && !repeat_owner {
            return Err(self.budget.fail(
                ErrorCode::IndexCorrupt,
                format!("canonical projection repeats a unique {family} row"),
            ));
        }
        if inserted != 0 {
            *self.counts.get_mut(family).expect("fixed checked family") += 1;
        }
        Ok(())
    }
    fn compare<P: Params>(
        &mut self,
        family: &'static str,
        columns: &str,
        predicate: &str,
        parameters: P,
        expected: &[ValueRef<'_>],
        repeat_owner: bool,
    ) -> Result<i64> {
        self.budget.guard()?;
        let rid = {
            // Table, columns and predicate are compile-time call-site constants.
            let query = format!("SELECT rowid,{columns} FROM {family} WHERE {predicate}");
            self.require_streaming_plan(&query)?;
            let mut statement = self
                .source
                .prepare(&query)
                .map_err(|e| self.budget.sql_error(e))?;
            let mut rows = statement
                .query(parameters)
                .map_err(|e| self.budget.sql_error(e))?;
            let row = rows.next().map_err(|e| self.budget.sql_error(e))?
                .ok_or_else(|| self.budget.fail(ErrorCode::IndexCorrupt, format!("selected index is missing a canonical {family} row; run index sync or rebuild")))?;
            let rid: i64 = row.get(0).map_err(|e| self.budget.sql_error(e))?;
            let mut bytes = 0u64;
            for (index, expected) in expected.iter().enumerate() {
                let actual = row
                    .get_ref(index + 1)
                    .map_err(|e| self.budget.sql_error(e))?;
                bytes = bytes
                    .checked_add(match actual {
                        ValueRef::Text(v) | ValueRef::Blob(v) => v.len() as u64,
                        _ => 8,
                    })
                    .ok_or_else(|| {
                        self.budget
                            .fail(ErrorCode::BudgetExceeded, "audit row byte count overflow")
                    })?;
                if actual != *expected {
                    return Err(self.budget.fail(ErrorCode::IndexCorrupt,
                        format!("selected {family} row differs from canonical projection; run index sync or rebuild")));
                }
            }
            self.budget.admit_row(bytes)?;
            if rows.next().map_err(|e| self.budget.sql_error(e))?.is_some() {
                return Err(self.budget.fail(
                    ErrorCode::IndexCorrupt,
                    format!("selected {family} logical key is not unique"),
                ));
            }
            rid
        };
        self.consume(family, rid, repeat_owner)?;
        Ok(rid)
    }
    fn duplicate<P: Params>(
        &mut self,
        family: &'static str,
        predicate: &str,
        parameters: P,
    ) -> Result<()> {
        let selected = {
            let query = format!("SELECT rowid FROM {family} WHERE {predicate}");
            self.require_streaming_plan(&query)?;
            let mut statement = self
                .source
                .prepare(&query)
                .map_err(|e| self.budget.sql_error(e))?;
            let mut rows = statement
                .query(parameters)
                .map_err(|e| self.budget.sql_error(e))?;
            let mut selected = None;
            while let Some(row) = rows.next().map_err(|e| self.budget.sql_error(e))? {
                self.budget.admit_row(8)?;
                let rid = row.get(0).map_err(|e| self.budget.sql_error(e))?;
                if !self.consumed(family, rid)? {
                    selected = Some(rid);
                    break;
                }
            }
            selected.ok_or_else(|| {
                self.budget.fail(
                    ErrorCode::IndexCorrupt,
                    format!("selected {family} multiplicity differs from canonical projection"),
                )
            })?
        };
        self.consume(family, selected, false)
    }

    pub(crate) fn finish(
        mut self,
        projection: &NormalizedValidationProjection,
        fs: &VaultFs,
        writer: &WriterPermit,
    ) -> Result<CompactStats> {
        normalized_metadata::visit_eligibility_rows(
            &projection.validation,
            &projection.facts,
            &mut self,
        )
        .map_err(|e| self.budget.fail(e.code, e.message))?;
        normalized_metadata::visit_refresh_rows(&projection.validation, &mut self)
            .map_err(|e| self.budget.fail(e.code, e.message))?;
        for (id, row) in &projection.validation.records {
            self.budget.guard()?;
            if id != row.record.id() {
                return Err(self.budget.fail(
                    ErrorCode::IndexCorrupt,
                    "canonical record key differs from identity",
                ));
            }
            let json = self.json(row)?;
            self.compare("records","id,kind,path,hash,authored_status,eligibility,identity_eligibility,description_eligibility,disputed,row_json",
                "id=?1",[id.as_str()],&[
                    text(id.as_str()),text(row.record.kind().as_str()),text(row.path.as_str()),text(row.hash.as_str()),optional(row.authored_status.as_deref()),
                    text(eligibility(row.eligibility)),optional(row.identity_eligibility.map(eligibility)),optional(row.description_eligibility.map(eligibility)),
                    ValueRef::Integer(i64::from(row.disputed)),text(&json),
                ],false)?;
        }
        for dependency in &projection.validation.dependencies {
            let hash = match &dependency.expected {
                ExpectedState::Absent => None,
                ExpectedState::Hash(hash) => Some(hash.as_str()),
            };
            self.compare(
                "dependencies",
                "path,expected_hash",
                "path=?1",
                [dependency.path.as_str()],
                &[text(dependency.path.as_str()), optional(hash)],
                false,
            )?;
        }
        for diagnostic in &projection.validation.diagnostics {
            self.budget.guard()?;
            let details = self.json(&diagnostic.details)?;
            self.duplicate(
                "diagnostics",
                "path=?1 AND record_id IS ?2 AND code=?3 AND details_json=?4",
                params![
                    diagnostic.path.as_str(),
                    diagnostic.record_id.as_ref().map(|id| id.as_str()),
                    diagnostic.code.to_string(),
                    details
                ],
            )?;
        }
        let engine = ChangeEngine::new(fs.clone())?;
        let budget = self.budget.clone();
        let mut steps = 0u64;
        engine
            .reconstruct_revision_owners(
                writer,
                None,
                &mut || {
                    budget.guard()?;
                    steps = steps
                        .checked_add(1)
                        .filter(|n| *n <= budget.limits().max_history_steps)
                        .ok_or_else(|| {
                            budget.fail(
                                ErrorCode::BudgetExceeded,
                                "explicit check history work limit exceeded",
                            )
                        })?;
                    Ok(())
                },
                &mut |owner| {
                    self.compare(
                        "revision_tree_owners",
                        "source_component,revision_component,change_id,manifest_hash",
                        "source_component=?1 AND revision_component=?2",
                        params![owner.key.source_component, owner.key.revision_component],
                        &[
                            text(&owner.key.source_component),
                            text(&owner.key.revision_component),
                            text(owner.change.change_id.as_str()),
                            text(owner.change.manifest_hash.as_str()),
                        ],
                        true,
                    )?;
                    Ok(())
                },
            )
            .map_err(|e| self.budget.fail(e.code, e.message))?;
        for family in FAMILIES {
            let expected = self.counts[family];
            self.budget.guard()?;
            self.require_streaming_plan(&format!("SELECT count(*) FROM {family}"))?;
            let count: i64 = self
                .source
                .query_row(&format!("SELECT count(*) FROM {family}"), [], |row| {
                    row.get(0)
                })
                .map_err(|e| self.budget.sql_error(e))?;
            if u64::try_from(count).ok() != Some(expected) {
                return Err(self.budget.fail(
                    ErrorCode::IndexCorrupt,
                    format!("selected {family} contains missing or extra rows"),
                ));
            }
        }
        self.compact.finish()
    }
}

impl RetrievalSink for AuditSink<'_> {
    fn identity_claim(&mut self, row: IdentityClaimRow) -> Result<()> {
        self.compare(
            "identity_claims",
            "record_id,path,file_hash,kind",
            "record_id=?1 AND path=?2",
            params![row.id.as_str(), row.path.as_str()],
            &[
                text(row.id.as_str()),
                text(row.path.as_str()),
                text(row.hash.as_str()),
                optional(row.kind.map(|k| k.as_str())),
            ],
            false,
        )?;
        Ok(())
    }
    fn document(&mut self, row: DocumentRow) -> Result<()> {
        counted_json(&row, self.budget.limits().max_row_bytes)
            .map_err(|e| self.budget.fail(e.code, e.message))?;
        let aliases = self.json(&row.aliases)?;
        let tags = self.json(&row.tags)?;
        let reasons = self.json(&row.reasons)?;
        let alias_text = row.aliases.join(" ");
        let tag_text = row.tags.join(" ");
        let rid=self.compare("documents","path,record_id,kind,file_hash,title,aliases_json,aliases_text,headings,tags_json,tags_text,body,raw_text,source_id,owner_revision,eligibility,reasons_json",
            "path=?1",[row.path.as_str()],&[
                text(row.path.as_str()),optional(row.record_id.as_ref().map(|v|v.as_str())),optional(row.kind.map(|k|k.as_str())),text(row.hash.as_str()),text(&row.title),
                text(&aliases),text(&alias_text),text(&row.headings),text(&tags),text(&tag_text),text(&row.body),text(&row.raw_text),
                optional(row.source_id.as_ref().map(|v|v.as_str())),optional(row.owner_revision.as_ref().map(|v|v.as_str())),text(eligibility(row.eligibility)),text(&reasons),
            ],false)?;
        self.compact.document(rid, &row)
    }
    fn graph(&mut self, row: GraphRow) -> Result<()> {
        counted_json(&row, self.budget.limits().max_row_bytes)
            .map_err(|e| self.budget.fail(e.code, e.message))?;
        let aliases = self.json(&row.aliases)?;
        let alias_text = row.aliases.join(" ");
        let rid=self.compare("graph_rows","target_id,target_kind,name,aliases_json,aliases_text,endpoints,predicate,qualifiers,description",
            "target_id=?1",[row.target_id.as_str()],&[
                text(row.target_id.as_str()),text(row.target_kind.as_str()),text(&row.name),text(&aliases),text(&alias_text),
                text(&row.endpoints),text(&row.predicate),text(&row.qualifiers),text(&row.description),
            ],false)?;
        self.compact.graph(rid, &row)
    }
    fn link(&mut self, row: LinkRow) -> Result<()> {
        self.duplicate("links","from_path=?1 AND byte_start=?2 AND target_id IS ?3 AND target_path IS ?4 AND resolution=?5",
            params![row.from_path.as_str(),sql::integer(row.byte_start)?,row.target_id.as_ref().map(|v|v.as_str()),row.target_path.as_ref().map(|v|v.as_str()),row.resolution])
    }
    fn link_fact(&mut self, row: OwnedLinkFact) -> Result<()> {
        self.compare(
            "link_facts",
            "from_path,byte_start,raw_destination,typed_id,typed_kind",
            "from_path=?1 AND byte_start=?2",
            params![row.from_path.as_str(), sql::integer(row.byte_start)?],
            &[
                text(row.from_path.as_str()),
                ValueRef::Integer(sql::integer(row.byte_start)?),
                text(&row.raw_destination),
                optional(row.typed.as_ref().map(|v| v.id.as_str())),
                optional(row.typed.as_ref().map(|v| v.expected_kind.as_str())),
            ],
            false,
        )?;
        for key in &row.keys {
            self.compare(
                "link_match_keys",
                "kind,value,from_path,byte_start",
                "kind=?1 AND value=?2 AND from_path=?3 AND byte_start=?4",
                params![
                    key_kind(key),
                    key.value,
                    row.from_path.as_str(),
                    sql::integer(row.byte_start)?
                ],
                &[
                    text(key_kind(key)),
                    text(&key.value),
                    text(row.from_path.as_str()),
                    ValueRef::Integer(sql::integer(row.byte_start)?),
                ],
                false,
            )?;
        }
        Ok(())
    }
    fn registry_keys(
        &mut self,
        entry: &crate::records::RegistryEntry,
        keys: &[MatchKey],
    ) -> Result<()> {
        for key in keys {
            self.compare(
                "registry_match_keys",
                "kind,value,record_id,path",
                "kind=?1 AND value=?2 AND record_id=?3 AND path=?4",
                params![
                    key_kind(key),
                    key.value,
                    entry.id.as_str(),
                    entry.path.as_str()
                ],
                &[
                    text(key_kind(key)),
                    text(&key.value),
                    text(entry.id.as_str()),
                    text(entry.path.as_str()),
                ],
                false,
            )?;
        }
        Ok(())
    }
}

impl MetadataSink for AuditSink<'_> {
    fn progress(&mut self) -> Result<()> {
        self.budget.guard()
    }
    fn row(&mut self, row: MetadataRow<'_>) -> Result<()> {
        match row {
            MetadataRow::Policy(row) => {
                let columns = row.columns()?;
                counted_json(&columns, self.budget.limits().max_row_bytes)?;
                self.compare(
                    "policy_facts",
                    "family,key,owner,value",
                    "family=?1 AND key=?2 AND owner=?3",
                    params![columns[0], columns[1], columns[2]],
                    &[
                        text(&columns[0]),
                        text(&columns[1]),
                        text(&columns[2]),
                        text(&columns[3]),
                    ],
                    false,
                )?;
            }
            MetadataRow::AssertionNavigation { assertion, key } => {
                self.compare(
                    "assertion_navigation_keys",
                    "kind,value,assertion_id",
                    "kind=?1 AND value=?2 AND assertion_id=?3",
                    params![key_kind(key), key.value, assertion.as_str()],
                    &[
                        text(key_kind(key)),
                        text(&key.value),
                        text(assertion.as_str()),
                    ],
                    false,
                )?;
            }
            MetadataRow::Opposition {
                assertion,
                key,
                negated,
            } => {
                let key = self.json(key)?;
                self.compare(
                    "opposition_members",
                    "key_json,negated,assertion_id",
                    "key_json=?1 AND negated=?2 AND assertion_id=?3",
                    params![key, negated, assertion.as_str()],
                    &[
                        text(&key),
                        ValueRef::Integer(i64::from(negated)),
                        text(assertion.as_str()),
                    ],
                    false,
                )?;
            }
            MetadataRow::Baseline {
                id,
                baseline,
                structural,
            } => {
                let baseline = self.json(baseline)?;
                let structural = self.json(structural)?;
                self.compare(
                    "record_eligibility_facts",
                    "record_id,baseline_json,structural_json",
                    "record_id=?1",
                    [id.as_str()],
                    &[text(id.as_str()), text(&baseline), text(&structural)],
                    false,
                )?;
            }
            MetadataRow::DirectPath { owner, path } => {
                self.compare(
                    "record_direct_paths",
                    "owner_id,path",
                    "owner_id=?1 AND path=?2",
                    params![owner.as_str(), path.as_str()],
                    &[text(owner.as_str()), text(path.as_str())],
                    false,
                )?;
            }
            MetadataRow::SemanticEdge(edge) => {
                let role = self.json(&edge.role)?;
                self.compare(
                    "semantic_edges",
                    "owner_id,target_id,role_json",
                    "owner_id=?1 AND target_id=?2 AND role_json=?3",
                    params![edge.owner_id.as_str(), edge.target_id.as_str(), role],
                    &[
                        text(edge.owner_id.as_str()),
                        text(edge.target_id.as_str()),
                        text(&role),
                    ],
                    false,
                )?;
            }
            MetadataRow::SourceRevision {
                source,
                revision,
                ordinal,
                original,
                content,
                fingerprint,
                status,
            } => {
                self.compare("source_revision_identity","source_id,revision_id,retained_ordinal,original_hash,content_hash,extractor_fingerprint,extraction_status",
                    "source_id=?1 AND revision_id=?2",params![source.as_str(),revision.as_str()],
                    &[text(source.as_str()),text(revision.as_str()),ValueRef::Integer(sql::integer(ordinal as u64)?),text(original),optional(content),text(fingerprint),text(status)],false)?;
            }
            MetadataRow::SourceEvidence {
                source,
                evidence,
                assertion,
            } => {
                self.compare(
                    "source_evidence",
                    "source_id,evidence_id,assertion_id",
                    "source_id=?1 AND evidence_id=?2",
                    params![source, evidence.as_str()],
                    &[text(source), text(evidence.as_str()), text(assertion)],
                    false,
                )?;
            }
        }
        Ok(())
    }
}

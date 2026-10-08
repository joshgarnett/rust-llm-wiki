//! Exact, replayable bounded row changes inside the caller's publication transaction.
//! No header, commit, filesystem or full-vault validation is performed here.
use super::{sql, types::*};
use crate::{
    changes::{ReadDependency, RevisionOwnerRow},
    domain::{
        Blake3Hash, Eligibility, ErrorCode, RecordId, RecordKind, Result, VaultRelativePath,
        WikiError,
    },
    vault::ExpectedState,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, io::Write};

pub(crate) const MAX_ROWS: usize = 4096;
pub(super) const MAX_ROW_BYTES: usize = 8 * 1024 * 1024;
pub(super) const MAX_DELTA_BYTES: usize = 256 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CatalogDelta {
    pub version: u32,
    pub records: Vec<RecordRow>,
    pub documents: Vec<DocumentMutation>,
    pub graph: Vec<GraphRow>,
    pub links: Vec<OwnedLinks>,
    pub diagnostics: Vec<OwnedDiagnostics>,
    pub claims: Vec<OwnedClaims>,
    pub revisions: Vec<RevisionIdentityRow>,
    pub dependencies: Vec<ReadDependency>,
    pub owners: Vec<RevisionOwnerRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facts: Option<super::normalized_fact_delta::FactDelta>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum DocumentMutation {
    /// Maintenance-only retirement in an unpublished sibling. The cached Page
    /// before-image must match; canonical Source and Revision owners are excluded.
    DeletePage {
        path: VaultRelativePath,
        expected_hash: Blake3Hash,
    },
    MovePage {
        from: VaultRelativePath,
        from_hash: Blake3Hash,
        row: DocumentRow,
    },
    Put {
        row: DocumentRow,
    },
    Metadata {
        path: VaultRelativePath,
        eligibility: Eligibility,
        reasons: Vec<String>,
    },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OwnedLinks {
    pub path: VaultRelativePath,
    pub rows: Vec<LinkRow>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OwnedDiagnostics {
    pub path: VaultRelativePath,
    pub rows: Vec<CatalogDiagnostic>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OwnedClaims {
    pub path: VaultRelativePath,
    pub rows: Vec<IdentityClaimRow>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RevisionIdentityRow {
    pub source_id: RecordId,
    pub revision_id: RecordId,
    pub retained_ordinal: usize,
    pub original_hash: Blake3Hash,
    pub content_hash: Option<Blake3Hash>,
    pub extractor_fingerprint: Blake3Hash,
    pub extraction_status: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DeltaStats {
    pub records: usize,
    pub documents: usize,
    pub graph_rows: usize,
    pub links: usize,
    pub diagnostics: usize,
    pub claims: usize,
    pub revisions: usize,
    pub dependencies: usize,
    pub owners: usize,
    pub old_rows: usize,
    pub old_fts_bytes: usize,
    pub old_fact_bytes: usize,
    pub fact_rows: usize,
}

impl CatalogDelta {
    pub fn validate(&self) -> Result<()> {
        if !matches!(
            (self.version, self.facts.is_some()),
            (1, false) | (2 | 3, true)
        ) {
            return Err(invalid(
                "catalog delta version and normalized facts disagree",
            ));
        }
        if self.version >= 2 && self.records.iter().any(|row| !row.dependencies.is_empty()) {
            return Err(invalid("normalized delta carries flattened record proofs"));
        }
        if self.version == 3
            && self
                .facts
                .as_ref()
                .and_then(|facts| facts.policy.as_ref())
                .is_none()
        {
            return Err(invalid(
                "normalized write delta requires complete policy maintenance",
            ));
        }
        let mut count = 0usize;
        let mut admit = |value: &dyn SizedJson| -> Result<()> {
            count = count
                .checked_add(1)
                .filter(|n| *n <= MAX_ROWS)
                .ok_or_else(|| budget("catalog delta exceeds row ceiling"))?;
            value.count(MAX_ROW_BYTES)?;
            Ok(())
        };
        let mut ids = BTreeSet::new();
        let mut paths = BTreeSet::new();
        for row in &self.records {
            admit(row)?;
            unique(&mut ids, row.record.id())?;
            unique(&mut paths, &row.path)?;
        }
        paths.clear();
        for action in &self.documents {
            admit(action)?;
            match action {
                DocumentMutation::Put { row } => {
                    unique(&mut paths, &row.path)?;
                    if row.source_id.is_some() != row.owner_revision.is_some() {
                        return Err(invalid("incomplete captured document ownership"));
                    }
                    if row.owner_revision.is_some()
                        && (row.record_id.is_some()
                            || row.kind.is_some()
                            || !row.aliases.is_empty()
                            || !row.tags.is_empty())
                    {
                        return Err(invalid("captured document carries canonical metadata"));
                    }
                }
                DocumentMutation::DeletePage { path, .. } => {
                    unique(&mut paths, path)?;
                    if self.version != 3
                        || !crate::sources::revision::canonical_path(path)
                        || self.records.iter().any(|r| &r.path == path)
                        || !self
                            .links
                            .iter()
                            .any(|o| &o.path == path && o.rows.is_empty())
                        || !self
                            .claims
                            .iter()
                            .any(|o| &o.path == path && o.rows.is_empty())
                        || !self
                            .diagnostics
                            .iter()
                            .any(|o| &o.path == path && o.rows.is_empty())
                        || !self.facts.as_ref().is_some_and(|f| {
                            f.links.iter().any(|o| &o.path == path && o.rows.is_empty())
                                && f.policy
                                    .as_ref()
                                    .is_some_and(|policy| policy.retired_owners.contains(path))
                        })
                    {
                        return Err(invalid(
                            "Page deletion requires a closed retired owner scope",
                        ));
                    }
                }
                DocumentMutation::MovePage { from, row, .. } => {
                    unique(&mut paths, from)?;
                    unique(&mut paths, &row.path)?;
                    if self.version != 3
                        || from == &row.path
                        || row.kind != Some(RecordKind::Page)
                        || row.record_id.is_none()
                        || row.source_id.is_some()
                        || row.owner_revision.is_some()
                        || !crate::sources::revision::canonical_path(from)
                        || !crate::sources::revision::canonical_path(&row.path)
                        || self
                            .documents
                            .iter()
                            .filter(|d| matches!(d, DocumentMutation::MovePage { .. }))
                            .count()
                            != 1
                        || !self.records.iter().any(|r| {
                            Some(r.record.id()) == row.record_id.as_ref()
                                && r.path == row.path
                                && r.hash == row.hash
                                && r.record.kind() == RecordKind::Page
                        })
                        || !self
                            .links
                            .iter()
                            .any(|o| &o.path == from && o.rows.is_empty())
                        || !self
                            .claims
                            .iter()
                            .any(|o| &o.path == from && o.rows.is_empty())
                        || !self
                            .diagnostics
                            .iter()
                            .any(|o| &o.path == from && o.rows.is_empty())
                        || !self.facts.as_ref().is_some_and(|f| {
                            f.links.iter().any(|o| &o.path == from && o.rows.is_empty())
                        })
                    {
                        return Err(invalid(
                            "Page move must bind one same-identity destination and retire old owned rows",
                        ));
                    }
                }
                DocumentMutation::Metadata { path, .. } => unique(&mut paths, path)?,
            }
        }
        ids.clear();
        for row in &self.graph {
            admit(row)?;
            unique(&mut ids, &row.target_id)?;
            if !matches!(row.target_kind, RecordKind::Entity | RecordKind::Assertion) {
                return Err(invalid("unsupported graph row kind"));
            }
        }
        paths.clear();
        for owned in &self.links {
            unique(&mut paths, &owned.path)?;
            admit(&owned.path)?;
            let mut keys = BTreeSet::new();
            for row in &owned.rows {
                admit(row)?;
                if row.from_path != owned.path {
                    return Err(invalid("cross-owned link"));
                }
                unique(&mut keys, row.byte_start)?;
                sql::integer(row.byte_start)?;
            }
        }
        paths.clear();
        for owned in &self.diagnostics {
            unique(&mut paths, &owned.path)?;
            admit(&owned.path)?;
            for row in &owned.rows {
                admit(row)?;
                if row.path != owned.path {
                    return Err(invalid("cross-owned diagnostic"));
                }
            }
        }
        paths.clear();
        for owned in &self.claims {
            unique(&mut paths, &owned.path)?;
            admit(&owned.path)?;
            let mut keys = BTreeSet::new();
            for row in &owned.rows {
                admit(row)?;
                if row.path != owned.path {
                    return Err(invalid("cross-owned identity claim"));
                }
                unique(&mut keys, &row.id)?;
            }
        }
        let mut revision_keys = BTreeSet::new();
        let mut ordinals = BTreeSet::new();
        for row in &self.revisions {
            admit(row)?;
            unique(&mut revision_keys, (&row.source_id, &row.revision_id))?;
            unique(&mut ordinals, (&row.source_id, row.retained_ordinal))?;
            sql::integer(row.retained_ordinal as u64)?;
            if !matches!(
                row.extraction_status.as_str(),
                "complete" | "unsupported" | "failed"
            ) {
                return Err(invalid("unknown extraction status"));
            }
        }
        paths.clear();
        for row in &self.dependencies {
            admit(row)?;
            unique(&mut paths, &row.path)?;
        }
        if self
            .dependencies
            .windows(2)
            .any(|rows| rows[0].path >= rows[1].path)
        {
            return Err(invalid("delta dependencies must be sorted by path"));
        }
        let mut keys = BTreeSet::new();
        for row in &self.owners {
            admit(row)?;
            unique(&mut keys, &row.key)?;
            for value in [&row.key.source_component, &row.key.revision_component] {
                if value.is_empty()
                    || value == "."
                    || value == ".."
                    || value.contains(['/', '\\', '\0'])
                {
                    return Err(invalid("invalid revision owner path component"));
                }
            }
        }
        if let Some(facts) = &self.facts {
            facts.validate(self, &mut count)?;
        }
        counted(self, MAX_DELTA_BYTES)?;
        Ok(())
    }

    /// Check the exact fact layout and access paths before canonical mutation,
    /// and again in the SQL transaction. A v1 plan cannot leave v1 facts stale.
    pub(super) fn require_layout(&self, connection: &Connection) -> Result<()> {
        let layout: i64 = connection
            .query_row(
                "SELECT proof_layout_version FROM catalog_meta WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .map_err(sql::sql_error)?;
        if layout != if self.version >= 2 { 2 } else { 0 } {
            return Err(invalid("catalog delta and selected proof layout disagree"));
        }
        if self.version == 3 {
            let layout: Option<String> = connection
                .query_row(
                    "SELECT value FROM policy_facts WHERE family='layout' AND key='' AND owner=''",
                    [],
                    |row| row.get(0),
                )
                .optional()
                .map_err(sql::sql_error)?;
            if layout.as_deref() != Some("1") {
                return Err(invalid(
                    "normalized write policy layout requires explicit rebuild",
                ));
            }
            connection.prepare("SELECT value FROM policy_facts INDEXED BY policy_facts_owner WHERE family=?1 AND owner=?2 AND key=?3").map_err(sql::sql_error)?;
        }
        if self.version >= 2 {
            for query in [
                "SELECT baseline_json,structural_json FROM record_eligibility_facts WHERE record_id=?1",
                "SELECT path FROM record_direct_paths WHERE owner_id=?1",
                "SELECT target_id FROM semantic_edges INDEXED BY semantic_outgoing WHERE owner_id=?1 AND role_json=?2",
                "SELECT owner_id FROM semantic_edges INDEXED BY semantic_dependents WHERE target_id=?1 AND role_json=?2",
                "SELECT raw_destination FROM link_facts WHERE from_path=?1",
                "SELECT value FROM link_match_keys INDEXED BY link_match_owners WHERE from_path=?1",
                "SELECT value FROM registry_match_keys INDEXED BY registry_match_owners WHERE record_id=?1",
                "SELECT assertion_id FROM opposition_members WHERE key_json=?1",
                "SELECT assertion_id FROM assertion_navigation_keys WHERE kind=?1 AND value=?2",
            ] {
                connection.prepare(query).map_err(sql::sql_error)?;
            }
        }
        Ok(())
    }

    /// Caller must hold BEGIN IMMEDIATE and configure cumulative VM/time limits.
    /// A savepoint reverses ordinary failed actions. SQLite can instead roll
    /// back the entire transaction on interruption or storage failure; callers
    /// must discard failed publication transactions in either case.
    pub fn apply(&self, connection: &Connection) -> Result<DeltaStats> {
        self.apply_for_operation(connection, None)
    }
    pub(crate) fn check_before_operation(
        &self,
        connection: &Connection,
        operation: Option<&crate::changes::indexed_refresh::IndexedWriteOperation>,
    ) -> Result<()> {
        self.check_page_move(connection, operation)?;
        self.check_page_deletions(connection, operation)?;
        if let Some(facts) = &self.facts {
            facts.check_before(connection, self, &mut DeltaStats::default(), operation)?;
        }
        Ok(())
    }
    fn check_page_deletions(
        &self,
        c: &Connection,
        operation: Option<&crate::changes::indexed_refresh::IndexedWriteOperation>,
    ) -> Result<()> {
        for action in &self.documents {
            let DocumentMutation::DeletePage {
                path,
                expected_hash,
            } = action
            else {
                continue;
            };
            let building: bool = c
                .query_row(
                    "SELECT state='building' FROM catalog_meta WHERE singleton=1",
                    [],
                    |r| r.get(0),
                )
                .map_err(sql::sql_error)?;
            if operation.is_some() || !building {
                return Err(invalid(
                    "Page deletion requires an unpublished maintenance sibling",
                ));
            }
            let valid: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM documents d JOIN records r ON r.id=d.record_id JOIN identity_claims i ON i.record_id=r.id AND i.path=r.path WHERE d.path=?1 AND d.file_hash=?2 AND d.kind='page' AND d.source_id IS NULL AND d.owner_revision IS NULL AND r.path=d.path AND r.hash=d.file_hash AND r.kind='page' AND i.file_hash=r.hash AND i.kind='page') AND (SELECT count(*) FROM identity_claims WHERE record_id=(SELECT record_id FROM documents WHERE path=?1))=1", params![path.as_str(),expected_hash.as_str()],|r|r.get(0)).map_err(sql::sql_error)?;
            if !valid {
                return Err(conflict(
                    "Page deletion before-image identity or ownership changed",
                ));
            }
            let raw: String = c
                .query_row(
                    "SELECT raw_text FROM documents WHERE path=?1",
                    [path.as_str()],
                    |r| r.get(0),
                )
                .map_err(sql::sql_error)?;
            if raw.len() > MAX_ROW_BYTES || Blake3Hash::digest(raw.as_bytes()) != *expected_hash {
                return Err(conflict(
                    "Page deletion cached before-image differs from its commitment",
                ));
            }
            let note = crate::records::parse_note(raw.as_bytes());
            let id: String = c
                .query_row(
                    "SELECT record_id FROM documents WHERE path=?1",
                    [path.as_str()],
                    |r| r.get(0),
                )
                .map_err(sql::sql_error)?;
            if note
                .canonical
                .as_ref()
                .is_none_or(|r| r.kind() != RecordKind::Page || r.id().as_str() != id)
            {
                return Err(conflict(
                    "Page deletion cached envelope differs from its owner",
                ));
            }
        }
        Ok(())
    }
    fn check_page_move(
        &self,
        c: &Connection,
        operation: Option<&crate::changes::indexed_refresh::IndexedWriteOperation>,
    ) -> Result<()> {
        use crate::changes::indexed_refresh::IndexedWriteOperation;
        let moves: Vec<_> = self
            .documents
            .iter()
            .filter_map(|d| {
                if let DocumentMutation::MovePage {
                    from,
                    from_hash,
                    row,
                } = d
                {
                    Some((from, from_hash, row))
                } else {
                    None
                }
            })
            .collect();
        match (moves.as_slice(), operation) {
            ([], Some(IndexedWriteOperation::PageRename { .. })) => {
                Err(invalid("Page rename lacks sealed document move"))
            }
            ([], _) => Ok(()),
            (
                [(from, from_hash, row)],
                Some(IndexedWriteOperation::PageRename {
                    page_id,
                    from: old_path,
                    to,
                    from_hash: old_hash,
                    ..
                }),
            ) if *from == old_path
                && *from_hash == old_hash
                && &row.path == to
                && row.record_id.as_ref() == Some(page_id) =>
            {
                let old: Option<(String, String, String)> = c
                    .query_row(
                        "SELECT kind,path,hash FROM records WHERE id=?1",
                        [page_id.as_str()],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .optional()
                    .map_err(sql::sql_error)?;
                if old
                    != Some((
                        RecordKind::Page.as_str().into(),
                        from.as_str().into(),
                        from_hash.as_str().into(),
                    ))
                {
                    return Err(conflict("Page move old identity/path/hash changed"));
                }
                let valid: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM documents WHERE path=?1 AND file_hash=?2 AND record_id=?3 AND kind='page' AND source_id IS NULL AND owner_revision IS NULL) AND NOT EXISTS(SELECT 1 FROM documents WHERE path=?4) AND NOT EXISTS(SELECT 1 FROM records WHERE path=?4) AND NOT EXISTS(SELECT 1 FROM identity_claims WHERE path=?4)",params![from.as_str(),from_hash.as_str(),page_id.as_str(),to.as_str()],|r|r.get(0)).map_err(sql::sql_error)?;
                if !valid {
                    return Err(conflict(
                        "Page move old document or absent destination changed",
                    ));
                }
                Ok(())
            }
            _ => Err(invalid(
                "document move lacks matching Page rename authority",
            )),
        }
    }
    pub(crate) fn apply_for_operation(
        &self,
        connection: &Connection,
        operation: Option<&crate::changes::indexed_refresh::IndexedWriteOperation>,
    ) -> Result<DeltaStats> {
        self.validate()?;
        self.require_layout(connection)?;
        if connection.is_autocommit() {
            return Err(invalid("catalog delta requires a publication transaction"));
        }
        connection
            .execute_batch("SAVEPOINT lwiki_catalog_delta")
            .map_err(sql::sql_error)?;
        let result = self.apply_rows(connection, operation);
        match result {
            Ok(stats) => {
                connection
                    .execute_batch("RELEASE lwiki_catalog_delta")
                    .map_err(sql::sql_error)?;
                Ok(stats)
            }
            Err(mut error) => {
                // Interrupted DML can already have rolled back the transaction.
                // Preserve the original budget/storage error, rather than mask
                // it with a missing-savepoint error during cleanup.
                if !connection.is_autocommit()
                    && let Err(cleanup) = connection.execute_batch(
                        "ROLLBACK TO lwiki_catalog_delta; RELEASE lwiki_catalog_delta",
                    )
                {
                    error.details = serde_json::json!({
                        "delta_cleanup_error": cleanup.to_string(),
                        "transaction_active": !connection.is_autocommit(),
                    });
                }
                Err(error)
            }
        }
    }
    fn apply_rows(
        &self,
        c: &Connection,
        operation: Option<&crate::changes::indexed_refresh::IndexedWriteOperation>,
    ) -> Result<DeltaStats> {
        let mut stats = DeltaStats::default();
        self.check_page_move(c, operation)?;
        self.check_page_deletions(c, operation)?;
        if let Some(facts) = &self.facts {
            facts.check_before(c, self, &mut stats, operation)?;
        }
        let affected_units = super::unit_inventory::affected_before(c, self)?;
        for document in &self.documents {
            if let DocumentMutation::MovePage {
                from,
                from_hash,
                row,
            } = document
            {
                if c.execute("UPDATE records SET path=?1 WHERE id=?2 AND kind='page' AND path=?3 AND hash=?4",params![row.path.as_str(),row.record_id.as_ref().unwrap().as_str(),from.as_str(),from_hash.as_str()]).map_err(sql::sql_error)? != 1
                    || c.execute("UPDATE documents SET path=?1 WHERE path=?2 AND file_hash=?3 AND record_id=?4",params![row.path.as_str(),from.as_str(),from_hash.as_str(),row.record_id.as_ref().unwrap().as_str()]).map_err(sql::sql_error)? != 1
                { return Err(conflict("Page move could not retire exact old location")); }
            }
        }
        for row in &self.records {
            let same: Option<bool> = c
                .query_row(
                    "SELECT kind=?2 AND path=?3 FROM records WHERE id=?1",
                    params![
                        row.record.id().as_str(),
                        row.record.kind().as_str(),
                        row.path.as_str()
                    ],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sql::sql_error)?;
            if same == Some(false) {
                return Err(conflict("record identity path or kind changed"));
            }
            c.execute("INSERT INTO records(id,kind,path,hash,authored_status,eligibility,identity_eligibility,description_eligibility,disputed,row_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) ON CONFLICT(id) DO UPDATE SET hash=excluded.hash,authored_status=excluded.authored_status,eligibility=excluded.eligibility,identity_eligibility=excluded.identity_eligibility,description_eligibility=excluded.description_eligibility,disputed=excluded.disputed,row_json=excluded.row_json",params![row.record.id().as_str(),row.record.kind().as_str(),row.path.as_str(),row.hash.as_str(),row.authored_status,eligibility(row.eligibility),row.identity_eligibility.map(eligibility),row.description_eligibility.map(eligibility),row.disputed,sql::json(row)?]).map_err(sql::sql_error)?;
            stats.records += 1;
        }
        for action in &self.documents {
            match action {
                DocumentMutation::DeletePage { path, .. } => delete_page(c, path, &mut stats)?,
                DocumentMutation::Put { row } | DocumentMutation::MovePage { row, .. } => {
                    put_document(c, row, &mut stats)?
                }
                DocumentMutation::Metadata {
                    path,
                    eligibility: value,
                    reasons,
                } => {
                    if c.execute(
                        "UPDATE documents SET eligibility=?2,reasons_json=?3 WHERE path=?1",
                        params![path.as_str(), eligibility(*value), sql::json(reasons)?],
                    )
                    .map_err(sql::sql_error)?
                        != 1
                    {
                        return Err(conflict("metadata document missing"));
                    }
                }
            }
            stats.documents += 1;
        }
        for row in &self.graph {
            put_graph(c, row, &mut stats)?;
            stats.graph_rows += 1;
        }
        for owned in &self.links {
            admit_owned(
                c,
                "SELECT link_row FROM links WHERE from_path=?1 LIMIT 4097",
                owned.path.as_str(),
                &mut stats,
            )?;
            c.execute(
                "DELETE FROM links WHERE from_path=?1",
                [owned.path.as_str()],
            )
            .map_err(sql::sql_error)?;
            for row in &owned.rows {
                c.execute("INSERT INTO links(from_path,byte_start,target_id,target_path,resolution) VALUES(?1,?2,?3,?4,?5)",params![row.from_path.as_str(),sql::integer(row.byte_start)?,row.target_id.as_ref().map(RecordId::as_str),row.target_path.as_ref().map(VaultRelativePath::as_str),row.resolution]).map_err(sql::sql_error)?;
                stats.links += 1;
            }
        }
        for owned in &self.diagnostics {
            admit_owned(
                c,
                "SELECT diagnostic_row FROM diagnostics WHERE path=?1 LIMIT 4097",
                owned.path.as_str(),
                &mut stats,
            )?;
            c.execute(
                "DELETE FROM diagnostics WHERE path=?1",
                [owned.path.as_str()],
            )
            .map_err(sql::sql_error)?;
            for row in &owned.rows {
                c.execute(
                    "INSERT INTO diagnostics(path,record_id,code,details_json) VALUES(?1,?2,?3,?4)",
                    params![
                        row.path.as_str(),
                        row.record_id.as_ref().map(RecordId::as_str),
                        row.code.to_string(),
                        sql::json(&row.details)?
                    ],
                )
                .map_err(sql::sql_error)?;
                stats.diagnostics += 1;
            }
        }
        for owned in &self.claims {
            admit_owned(
                c,
                "SELECT record_id FROM identity_claims INDEXED BY identity_claim_paths WHERE path=?1 LIMIT 4097",
                owned.path.as_str(),
                &mut stats,
            )?;
            c.execute(
                "DELETE FROM identity_claims WHERE path=?1",
                [owned.path.as_str()],
            )
            .map_err(sql::sql_error)?;
            for row in &owned.rows {
                c.execute("INSERT INTO identity_claims(record_id,path,file_hash,kind) VALUES(?1,?2,?3,?4)",params![row.id.as_str(),row.path.as_str(),row.hash.as_str(),row.kind.map(RecordKind::as_str)]).map_err(sql::sql_error)?;
                stats.claims += 1;
            }
        }
        for row in &self.revisions {
            let ordinal = sql::integer(row.retained_ordinal as u64)?;
            let changed=c.execute("INSERT INTO source_revision_identity(source_id,revision_id,retained_ordinal,original_hash,content_hash,extractor_fingerprint,extraction_status) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(source_id,revision_id) DO NOTHING",params![row.source_id.as_str(),row.revision_id.as_str(),ordinal,row.original_hash.as_str(),row.content_hash.as_ref().map(Blake3Hash::as_str),row.extractor_fingerprint.as_str(),row.extraction_status]).map_err(sql::sql_error)?;
            if changed == 0 {
                let same:bool=c.query_row("SELECT retained_ordinal=?3 AND original_hash=?4 AND content_hash IS ?5 AND extractor_fingerprint=?6 AND extraction_status=?7 FROM source_revision_identity WHERE source_id=?1 AND revision_id=?2",params![row.source_id.as_str(),row.revision_id.as_str(),ordinal,row.original_hash.as_str(),row.content_hash.as_ref().map(Blake3Hash::as_str),row.extractor_fingerprint.as_str(),row.extraction_status],|r|r.get(0)).map_err(sql::sql_error)?;
                if !same {
                    return Err(conflict("immutable revision signature changed"));
                }
            }
            stats.revisions += 1;
        }
        for row in &self.dependencies {
            let hash = match &row.expected {
                ExpectedState::Absent => None,
                ExpectedState::Hash(hash) => Some(hash.as_str()),
            };
            c.execute("INSERT INTO dependencies(path,expected_hash) VALUES(?1,?2) ON CONFLICT(path) DO UPDATE SET expected_hash=excluded.expected_hash",params![row.path.as_str(),hash]).map_err(sql::sql_error)?;
            stats.dependencies += 1;
        }
        for row in &self.owners {
            let changed=c.execute("INSERT INTO revision_tree_owners(source_component,revision_component,change_id,manifest_hash) VALUES(?1,?2,?3,?4) ON CONFLICT(source_component,revision_component) DO NOTHING",params![row.key.source_component,row.key.revision_component,row.change.change_id.as_str(),row.change.manifest_hash.as_str()]).map_err(sql::sql_error)?;
            if changed == 0 {
                let same:bool=c.query_row("SELECT change_id=?3 AND manifest_hash=?4 FROM revision_tree_owners WHERE source_component=?1 AND revision_component=?2",params![row.key.source_component,row.key.revision_component,row.change.change_id.as_str(),row.change.manifest_hash.as_str()],|r|r.get(0)).map_err(sql::sql_error)?;
                if !same {
                    return Err(conflict("immutable revision tree has another owner"));
                }
            }
            stats.owners += 1;
        }
        if let Some(facts) = &self.facts {
            facts.apply(c, self, &mut stats)?;
        }
        // Retained absence guards describe the move's filesystem closure. The
        // rebuildable relation retains an old path only for a surviving actual
        // asset/policy dependency, never merely for an incoming link lookup key.
        for document in &self.documents {
            let retired = match document {
                DocumentMutation::MovePage { from, .. } => Some(from),
                DocumentMutation::DeletePage { path, .. } => Some(path),
                _ => None,
            };
            if let Some(from) = retired {
                let referenced: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM record_direct_paths WHERE path=?1) OR EXISTS(SELECT 1 FROM policy_facts WHERE family='read_path' AND key=?1)", [from.as_str()], |r| r.get(0)).map_err(sql::sql_error)?;
                if !referenced {
                    admit_owned(
                        c,
                        "SELECT path FROM dependencies WHERE path=?1",
                        from.as_str(),
                        &mut stats,
                    )?;
                    c.execute("DELETE FROM dependencies WHERE path=?1", [from.as_str()])
                        .map_err(sql::sql_error)?;
                }
            }
        }
        super::unit_inventory::apply_after(c, self, &affected_units)?;
        Ok(stats)
    }
}

fn delete_page(c: &Connection, path: &VaultRelativePath, stats: &mut DeltaStats) -> Result<()> {
    let mut statement=c.prepare("SELECT doc_row,record_id,title,aliases_text,headings,tags_text,body,length(CAST(title AS BLOB)),length(CAST(aliases_text AS BLOB)),length(CAST(headings AS BLOB)),length(CAST(tags_text AS BLOB)),length(CAST(body AS BLOB)) FROM documents WHERE path=?1").map_err(sql::sql_error)?;
    let mut rows = statement.query([path.as_str()]).map_err(sql::sql_error)?;
    let row = rows
        .next()
        .map_err(sql::sql_error)?
        .ok_or_else(|| conflict("deleted Page document missing"))?;
    admit_old(row, 7, 5, stats)?;
    let doc_row: i64 = row.get(0).map_err(sql::sql_error)?;
    let id: String = row.get(1).map_err(sql::sql_error)?;
    c.execute("INSERT INTO documents_fts(documents_fts,rowid,title,aliases,headings,tags,body) VALUES('delete',?1,?2,?3,?4,?5,?6)",params![doc_row,text(row,2)?,text(row,3)?,text(row,4)?,text(row,5)?,text(row,6)?]).map_err(sql::sql_error)?;
    drop(rows);
    drop(statement);
    for (table, column) in [
        ("record_eligibility_facts", "record_id"),
        ("record_direct_paths", "owner_id"),
        ("semantic_edges", "owner_id"),
        ("registry_match_keys", "record_id"),
        ("opposition_members", "assertion_id"),
        ("assertion_navigation_keys", "assertion_id"),
    ] {
        let query = format!("SELECT * FROM {table} WHERE {column}=?1 LIMIT 4097");
        let mut statement = c.prepare(&query).map_err(sql::sql_error)?;
        let columns = statement.column_count();
        let mut rows = statement.query([id.as_str()]).map_err(sql::sql_error)?;
        while let Some(row) = rows.next().map_err(sql::sql_error)? {
            let mut bytes = 0usize;
            for column in 0..columns {
                if let rusqlite::types::ValueRef::Text(text) =
                    row.get_ref(column).map_err(sql::sql_error)?
                {
                    bytes = bytes
                        .checked_add(text.len())
                        .ok_or_else(|| budget("Page retirement byte overflow"))?;
                }
            }
            super::normalized_fact_delta::charge_old(bytes, stats)?;
        }
        drop(rows);
        drop(statement);
        c.execute(
            &format!("DELETE FROM {table} WHERE {column}=?1"),
            [id.as_str()],
        )
        .map_err(sql::sql_error)?;
    }
    let units: bool=c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='unit_policies')",[],|row|row.get(0)).map_err(sql::sql_error)?;
    if units {
        for table in ["unit_owners", "retrieval_units", "unit_owner_dependencies"] {
            let mut statement = c
                .prepare(&format!("SELECT * FROM {table} WHERE owner=?1 LIMIT 4097"))
                .map_err(sql::sql_error)?;
            let columns = statement.column_count();
            let mut rows = statement.query([path.as_str()]).map_err(sql::sql_error)?;
            while let Some(row) = rows.next().map_err(sql::sql_error)? {
                let mut bytes = 0usize;
                for column in 0..columns {
                    if let rusqlite::types::ValueRef::Text(text) =
                        row.get_ref(column).map_err(sql::sql_error)?
                    {
                        bytes = bytes
                            .checked_add(text.len())
                            .ok_or_else(|| budget("retired unit row byte overflow"))?;
                    }
                }
                super::normalized_fact_delta::charge_old(bytes, stats)?;
            }
        }
    }
    c.execute(
        "DELETE FROM records WHERE id=?1 AND kind='page'",
        [id.as_str()],
    )
    .map_err(sql::sql_error)?;
    c.execute("DELETE FROM documents WHERE path=?1", [path.as_str()])
        .map_err(sql::sql_error)?;
    Ok(())
}

fn put_document(c: &Connection, row: &DocumentRow, stats: &mut DeltaStats) -> Result<()> {
    let mut s=c.prepare("SELECT doc_row,title,aliases_text,headings,tags_text,body,length(CAST(title AS BLOB)),length(CAST(aliases_text AS BLOB)),length(CAST(headings AS BLOB)),length(CAST(tags_text AS BLOB)),length(CAST(body AS BLOB)),record_id IS ?2 AND kind IS ?3 AND source_id IS ?4 AND owner_revision IS ?5 FROM documents WHERE path=?1").map_err(sql::sql_error)?;
    let mut rows = s
        .query(params![
            row.path.as_str(),
            row.record_id.as_ref().map(RecordId::as_str),
            row.kind.map(RecordKind::as_str),
            row.source_id.as_ref().map(RecordId::as_str),
            row.owner_revision.as_ref().map(RecordId::as_str)
        ])
        .map_err(sql::sql_error)?;
    let old = rows.next().map_err(sql::sql_error)?;
    let rowid = if let Some(old) = old {
        admit_old(old, 6, 5, stats)?;
        let same: bool = old.get(11).map_err(sql::sql_error)?;
        if !same {
            return Err(conflict("document identity or ownership changed"));
        }
        let id: i64 = old.get(0).map_err(sql::sql_error)?;
        c.execute("INSERT INTO documents_fts(documents_fts,rowid,title,aliases,headings,tags,body) VALUES('delete',?1,?2,?3,?4,?5,?6)",params![id,text(old,1)?,text(old,2)?,text(old,3)?,text(old,4)?,text(old,5)?]).map_err(sql::sql_error)?;
        Some(id)
    } else {
        None
    };
    drop(rows);
    drop(s);
    let aliases = row.aliases.join(" ");
    let tags = row.tags.join(" ");
    c.execute("INSERT INTO documents(doc_row,path,record_id,kind,file_hash,title,aliases_json,aliases_text,headings,tags_json,tags_text,body,raw_text,source_id,owner_revision,eligibility,reasons_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17) ON CONFLICT(path) DO UPDATE SET record_id=excluded.record_id,kind=excluded.kind,file_hash=excluded.file_hash,title=excluded.title,aliases_json=excluded.aliases_json,aliases_text=excluded.aliases_text,headings=excluded.headings,tags_json=excluded.tags_json,tags_text=excluded.tags_text,body=excluded.body,raw_text=excluded.raw_text,source_id=excluded.source_id,owner_revision=excluded.owner_revision,eligibility=excluded.eligibility,reasons_json=excluded.reasons_json",params![rowid,row.path.as_str(),row.record_id.as_ref().map(RecordId::as_str),row.kind.map(RecordKind::as_str),row.hash.as_str(),row.title,sql::json(&row.aliases)?,aliases,row.headings,sql::json(&row.tags)?,tags,row.body,row.raw_text,row.source_id.as_ref().map(RecordId::as_str),row.owner_revision.as_ref().map(RecordId::as_str),eligibility(row.eligibility),sql::json(&row.reasons)?]).map_err(sql::sql_error)?;
    let id = rowid.unwrap_or_else(|| c.last_insert_rowid());
    c.execute("INSERT INTO documents_fts(rowid,title,aliases,headings,tags,body) VALUES(?1,?2,?3,?4,?5,?6)",params![id,row.title,aliases,row.headings,tags,row.body]).map_err(sql::sql_error)?;
    Ok(())
}
fn put_graph(c: &Connection, row: &GraphRow, stats: &mut DeltaStats) -> Result<()> {
    let mut s=c.prepare("SELECT graph_row,name,aliases_text,endpoints,predicate,qualifiers,description,target_kind,target_id,length(CAST(name AS BLOB)),length(CAST(aliases_text AS BLOB)),length(CAST(endpoints AS BLOB)),length(CAST(predicate AS BLOB)),length(CAST(qualifiers AS BLOB)),length(CAST(description AS BLOB)),length(CAST(target_kind AS BLOB)),length(CAST(target_id AS BLOB)) FROM graph_rows WHERE target_id=?1").map_err(sql::sql_error)?;
    let mut rows = s.query([row.target_id.as_str()]).map_err(sql::sql_error)?;
    let old = rows.next().map_err(sql::sql_error)?;
    let rowid = if let Some(old) = old {
        admit_old(old, 9, 8, stats)?;
        if text(old, 7)? != row.target_kind.as_str() {
            return Err(conflict("graph target kind changed"));
        }
        let id: i64 = old.get(0).map_err(sql::sql_error)?;
        c.execute("INSERT INTO graph_fts(graph_fts,rowid,name,aliases,endpoints,predicate,qualifiers,description,target_kind,target_id) VALUES('delete',?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![id,text(old,1)?,text(old,2)?,text(old,3)?,text(old,4)?,text(old,5)?,text(old,6)?,text(old,7)?,text(old,8)?]).map_err(sql::sql_error)?;
        Some(id)
    } else {
        None
    };
    drop(rows);
    drop(s);
    let aliases = row.aliases.join(" ");
    c.execute("INSERT INTO graph_rows(graph_row,target_id,target_kind,name,aliases_json,aliases_text,endpoints,predicate,qualifiers,description) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) ON CONFLICT(target_id) DO UPDATE SET name=excluded.name,aliases_json=excluded.aliases_json,aliases_text=excluded.aliases_text,endpoints=excluded.endpoints,predicate=excluded.predicate,qualifiers=excluded.qualifiers,description=excluded.description",params![rowid,row.target_id.as_str(),row.target_kind.as_str(),row.name,sql::json(&row.aliases)?,aliases,row.endpoints,row.predicate,row.qualifiers,row.description]).map_err(sql::sql_error)?;
    let id = rowid.unwrap_or_else(|| c.last_insert_rowid());
    c.execute("INSERT INTO graph_fts(rowid,name,aliases,endpoints,predicate,qualifiers,description,target_kind,target_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![id,row.name,aliases,row.endpoints,row.predicate,row.qualifiers,row.description,row.target_kind.as_str(),row.target_id.as_str()]).map_err(sql::sql_error)?;
    Ok(())
}
fn admit_old(
    row: &rusqlite::Row<'_>,
    offset: usize,
    count: usize,
    stats: &mut DeltaStats,
) -> Result<()> {
    let mut bytes = 0usize;
    for i in offset..offset + count {
        let value: i64 = row.get(i).map_err(sql::sql_error)?;
        bytes = bytes
            .checked_add(
                usize::try_from(value).map_err(|_| invalid("invalid old derivative size"))?,
            )
            .ok_or_else(|| budget("old derivative size overflow"))?;
    }
    if bytes > MAX_ROW_BYTES {
        return Err(budget("old derivative exceeds row byte ceiling"));
    }
    stats.old_fts_bytes = stats
        .old_fts_bytes
        .checked_add(bytes)
        .filter(|n| {
            n.checked_add(stats.old_fact_bytes)
                .is_some_and(|total| total <= MAX_DELTA_BYTES)
        })
        .ok_or_else(|| budget("old derivatives exceed byte ceiling"))?;
    stats.old_rows = stats
        .old_rows
        .checked_add(1)
        .filter(|n| *n <= MAX_ROWS)
        .ok_or_else(|| budget("old rows exceed count ceiling"))?;
    Ok(())
}
fn text<'a>(row: &'a rusqlite::Row<'_>, column: usize) -> Result<&'a str> {
    row.get_ref(column)
        .map_err(sql::sql_error)?
        .as_str()
        .map_err(|_| invalid("old derivative is not UTF-8 text"))
}
fn admit_owned(c: &Connection, query: &str, path: &str, stats: &mut DeltaStats) -> Result<()> {
    let mut s = c.prepare(query).map_err(sql::sql_error)?;
    let mut rows = s.query([path]).map_err(sql::sql_error)?;
    while rows.next().map_err(sql::sql_error)?.is_some() {
        stats.old_rows = stats
            .old_rows
            .checked_add(1)
            .filter(|n| *n <= MAX_ROWS)
            .ok_or_else(|| budget("owned old rows exceed ceiling"))?;
    }
    Ok(())
}
fn eligibility(value: Eligibility) -> &'static str {
    match value {
        Eligibility::Current => "current",
        Eligibility::Historical => "historical",
        Eligibility::Stale => "stale",
        Eligibility::Withdrawn => "withdrawn",
        Eligibility::Unsupported => "unsupported",
        Eligibility::Invalid => "invalid",
    }
}
fn unique<T: Ord>(keys: &mut BTreeSet<T>, key: T) -> Result<()> {
    if keys.insert(key) {
        Ok(())
    } else {
        Err(invalid("duplicate delta-owned key"))
    }
}
trait SizedJson {
    fn count(&self, limit: usize) -> Result<usize>;
}
impl<T: Serialize> SizedJson for T {
    fn count(&self, limit: usize) -> Result<usize> {
        counted(self, limit)
    }
}
struct Counter {
    bytes: usize,
    limit: usize,
}
impl Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .filter(|n| *n <= self.limit)
            .ok_or_else(|| std::io::Error::other("catalog delta byte ceiling"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(crate) fn counted<T: Serialize>(value: &T, limit: usize) -> Result<usize> {
    let mut counter = Counter { bytes: 0, limit };
    serde_json::to_writer(&mut counter, value)
        .map_err(|_| budget("catalog delta exceeds byte ceiling"))?;
    Ok(counter.bytes)
}
fn invalid(message: &str) -> WikiError {
    WikiError::new(ErrorCode::IndexCorrupt, message)
}
fn conflict(message: &str) -> WikiError {
    WikiError::new(ErrorCode::ContentConflict, message)
}
fn budget(message: &str) -> WikiError {
    WikiError::new(ErrorCode::BudgetExceeded, message)
}

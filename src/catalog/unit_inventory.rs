//! Disposable compact units. Canonical selected proofs remain the authority.
use super::{
    Catalog, DocumentRow,
    normalized_delta::{CatalogDelta, DocumentMutation},
    normalized_read, normalized_schema,
    query::QuerySnapshot,
    query_types::{QueryCatalog, QueryReadLimits},
    selector, sql,
};
use crate::{
    changes::ReadDependency,
    domain::*,
    retrieval::{
        indexed_units::{UnitBudget, UnitLimits, render_owner},
        spaces::EmbeddingSettings,
        unit_inventory_types::*,
    },
    vault::{ExpectedState, WriterPermit},
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use std::{collections::BTreeSet, time::Duration};

#[cfg(test)]
#[path = "unit_inventory_tests.rs"]
mod tests;

pub(crate) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS unit_policies(
 policy TEXT PRIMARY KEY,version INTEGER NOT NULL CHECK(version=1),
 parser_hash TEXT NOT NULL,settings_json TEXT NOT NULL,
 complete INTEGER NOT NULL CHECK(complete IN (0,1)),after_owner TEXT,
 unit_count INTEGER NOT NULL DEFAULT 0 CHECK(unit_count>=0));
CREATE TABLE IF NOT EXISTS unit_owners(
 policy TEXT NOT NULL,owner TEXT COLLATE BINARY NOT NULL,source_hash TEXT NOT NULL,
 render_token TEXT NOT NULL,proof_version INTEGER NOT NULL CHECK(proof_version>0),
 modified_seq INTEGER NOT NULL CHECK(modified_seq>0),
 tombstone INTEGER NOT NULL CHECK(tombstone IN (0,1)),unit_count INTEGER NOT NULL CHECK(unit_count>=0),
 PRIMARY KEY(policy,owner));
CREATE INDEX IF NOT EXISTS unit_owner_changes ON unit_owners(policy,modified_seq,owner);
CREATE TABLE IF NOT EXISTS retrieval_units(
 policy TEXT NOT NULL,owner TEXT NOT NULL,unit_id TEXT NOT NULL,input_hash TEXT NOT NULL,
 descriptor_json TEXT NOT NULL,PRIMARY KEY(policy,unit_id));
CREATE INDEX IF NOT EXISTS retrieval_unit_owners ON retrieval_units(policy,owner,unit_id);
CREATE TABLE IF NOT EXISTS unit_owner_dependencies(
 policy TEXT NOT NULL,owner TEXT NOT NULL,path TEXT NOT NULL,
 PRIMARY KEY(policy,owner,path));
CREATE INDEX IF NOT EXISTS unit_dependency_owners ON unit_owner_dependencies(path,policy,owner);
"#;

fn corrupt(message: &str) -> WikiError {
    WikiError::new(ErrorCode::IndexCorrupt, message)
}
fn exhausted(message: &str) -> WikiError {
    WikiError::new(ErrorCode::BudgetExceeded, message)
}
fn present(c: &Connection) -> Result<bool> {
    c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='unit_policies')",
        [],
        |r| r.get(0),
    )
    .map_err(sql::sql_error)
}
fn text<'a>(r: &'a rusqlite::Row<'_>, column: usize) -> Result<&'a str> {
    r.get_ref(column)
        .map_err(sql::sql_error)?
        .as_str()
        .map_err(|_| corrupt("unit inventory scalar is not text"))
}
fn decode_descriptor(r: &rusqlite::Row<'_>, policy: &RenderPolicyId) -> Result<UnitDescriptor> {
    let value: UnitDescriptor = serde_json::from_str(text(r, 3)?)
        .map_err(|_| corrupt("invalid compact unit descriptor"))?;
    if &value.policy != policy
        || value.unit_id.as_str() != text(r, 0)?
        || value.owner.as_str() != text(r, 1)?
        || value.input_hash.as_str() != text(r, 2)?
        || value.target != crate::retrieval::render::TargetKind::Document
    {
        return Err(corrupt("unit descriptor differs from its indexed identity"));
    }
    Ok(value)
}

impl QuerySnapshot {
    pub(crate) fn unit_changed_count(&self, policy: &RenderPolicyId, since: u64) -> Result<usize> {
        let mut statement = self.connection().prepare("SELECT COALESCE(sum(unit_count),0) FROM unit_owners INDEXED BY unit_owner_changes WHERE policy=?1 AND modified_seq>?2").map_err(sql::sql_error)?;
        let mut rows = statement
            .query(params![policy.as_str(), sql::integer(since)?])
            .map_err(sql::sql_error)?;
        let row = rows
            .next()
            .map_err(sql::sql_error)?
            .ok_or_else(|| corrupt("unit owner count absent"))?;
        self.reserve_scalar_row(row, 1)?;
        usize::try_from(row.get::<_, i64>(0).map_err(sql::sql_error)?)
            .map_err(|_| corrupt("invalid changed-unit count"))
    }
    pub(crate) fn unit_inventory_state(
        &self,
        policy: &RenderPolicyId,
    ) -> Result<Option<UnitInventoryState>> {
        // Capability inspection is itself charged; absence on an old cache is
        // explicit and never interpreted as a complete empty corpus.
        let mut s = self
            .connection()
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='unit_policies'")
            .map_err(sql::sql_error)?;
        let mut rows = s.query([]).map_err(sql::sql_error)?;
        let Some(row) = rows.next().map_err(sql::sql_error)? else {
            return Ok(None);
        };
        self.reserve_scalar_row(row, 1)?;
        drop(rows);
        drop(s);
        let mut s = self.connection().prepare("SELECT version,parser_hash,settings_json,complete,after_owner,unit_count FROM unit_policies WHERE policy=?1").map_err(sql::sql_error)?;
        let mut rows = s.query([policy.as_str()]).map_err(sql::sql_error)?;
        let Some(row) = rows.next().map_err(sql::sql_error)? else {
            return Ok(None);
        };
        self.reserve_scalar_row(row, 6)?;
        let settings: EmbeddingSettings = serde_json::from_str(text(row, 2)?)
            .map_err(|_| corrupt("invalid inventory render settings"))?;
        if row.get::<_, i64>(0).map_err(sql::sql_error)? != i64::from(INVENTORY_VERSION)
            || text(row, 1)? != self.snapshot().parser_fingerprint.as_str()
            || RenderPolicyId::for_settings(&self.snapshot().parser_fingerprint, &settings)?
                != *policy
        {
            return Err(corrupt("unit policy identity/settings/version mismatch"));
        }
        let complete = match row.get::<_, i64>(3).map_err(sql::sql_error)? {
            0 => false,
            1 => true,
            _ => return Err(corrupt("invalid inventory completion flag")),
        };
        let after_owner = row
            .get::<_, Option<String>>(4)
            .map_err(sql::sql_error)?
            .map(VaultRelativePath::new)
            .transpose()?;
        let total: i64 = row.get(5).map_err(sql::sql_error)?;
        let unit_count =
            usize::try_from(total).map_err(|_| corrupt("invalid inventory unit count"))?;
        Ok(Some(UnitInventoryState {
            policy: policy.clone(),
            complete,
            after_owner,
            through_seq: self.snapshot().generation,
            unit_count,
        }))
    }
    pub(crate) fn unit_descriptors_page(
        &self,
        policy: &RenderPolicyId,
        after: Option<&Blake3Hash>,
        limit: usize,
    ) -> Result<Vec<UnitDescriptor>> {
        if !(1..=128).contains(&limit) {
            return Err(WikiError::invalid(
                "compact unit page requires 1..=128 units",
            ));
        }
        let mut s = self.connection().prepare(
            "SELECT u.unit_id,u.owner,u.input_hash,u.descriptor_json FROM retrieval_units u \
             JOIN unit_owners o ON o.policy=u.policy AND o.owner=u.owner \
             JOIN documents d ON d.path=o.owner \
             WHERE u.policy=?1 AND u.unit_id>?2 AND o.tombstone=0 AND d.eligibility='current' \
             AND (d.kind IS NULL OR d.kind='page') AND d.file_hash=o.source_hash ORDER BY u.unit_id LIMIT ?3"
        ).map_err(sql::sql_error)?;
        let cursor = after.map_or("", Blake3Hash::as_str);
        let mut rows = s
            .query(params![policy.as_str(), cursor, limit as i64])
            .map_err(sql::sql_error)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(sql::sql_error)? {
            self.reserve_scalar_row(row, 4)?;
            let unit = decode_descriptor(row, policy)?;
            if unit.unit_id.as_str() <= cursor
                || out
                    .last()
                    .is_some_and(|last: &UnitDescriptor| last.unit_id >= unit.unit_id)
            {
                return Err(corrupt("compact unit cursor order changed"));
            }
            out.push(unit);
        }
        Ok(out)
    }
    pub(crate) fn unit_descriptors_for_owner(
        &self,
        policy: &RenderPolicyId,
        owner: &VaultRelativePath,
        limit: usize,
    ) -> Result<Vec<UnitDescriptor>> {
        if !(1..=4096).contains(&limit) {
            return Err(WikiError::invalid(
                "selected compact owner requires 1..=4096 units",
            ));
        }
        let mut s = self.connection().prepare("SELECT unit_id,owner,input_hash,descriptor_json FROM retrieval_units INDEXED BY retrieval_unit_owners WHERE policy=?1 AND owner=?2 ORDER BY unit_id LIMIT ?3").map_err(sql::sql_error)?;
        let mut rows = s
            .query(params![policy.as_str(), owner.as_str(), (limit + 1) as i64])
            .map_err(sql::sql_error)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(sql::sql_error)? {
            self.reserve_scalar_row(row, 4)?;
            if out.len() == limit {
                return Err(exhausted("selected owner compact unit limit exceeded"));
            }
            let unit = decode_descriptor(row, policy)?;
            if &unit.owner != owner {
                return Err(corrupt("compact lookup escaped selected owner"));
            }
            out.push(unit);
        }
        Ok(out)
    }
    fn decode_unit_owner(
        &self,
        row: &rusqlite::Row<'_>,
        policy: &RenderPolicyId,
    ) -> Result<UnitOwnerBinding> {
        self.reserve_scalar_row(row, 6)?;
        let proof: i64 = row.get(2).map_err(sql::sql_error)?;
        let seq: i64 = row.get(3).map_err(sql::sql_error)?;
        let tombstone: i64 = row.get(4).map_err(sql::sql_error)?;
        let count: i64 = row.get(5).map_err(sql::sql_error)?;
        if proof <= 0
            || seq <= 0
            || proof as u64 > self.snapshot().generation
            || seq as u64 > self.snapshot().generation
            || !matches!(tombstone, 0 | 1)
            || count < 0
        {
            return Err(corrupt("invalid unit owner version/count"));
        }
        Ok(UnitOwnerBinding {
            incarnation: catalog_incarnation(self.vault_id(), self.snapshot())?,
            policy: policy.clone(),
            owner: VaultRelativePath::new(text(row, 0)?)?,
            render_token: Blake3Hash::new(text(row, 1)?)?,
            proof_version: proof as u64,
            modified_seq: seq as u64,
            tombstone: tombstone == 1,
            unit_count: usize::try_from(count)
                .map_err(|_| corrupt("unit count exceeds host size"))?,
        })
    }
    pub(crate) fn unit_owner_binding(
        &self,
        policy: &RenderPolicyId,
        owner: &VaultRelativePath,
    ) -> Result<Option<UnitOwnerBinding>> {
        let mut s = self.connection().prepare("SELECT owner,render_token,proof_version,modified_seq,tombstone,unit_count FROM unit_owners WHERE policy=?1 AND owner=?2").map_err(sql::sql_error)?;
        let mut rows = s
            .query(params![policy.as_str(), owner.as_str()])
            .map_err(sql::sql_error)?;
        rows.next()
            .map_err(sql::sql_error)?
            .map(|r| self.decode_unit_owner(r, policy))
            .transpose()
    }
    pub(crate) fn unit_owner_bindings_page(
        &self,
        policy: &RenderPolicyId,
        after: Option<&UnitOwnerCursor>,
        since: u64,
        through: u64,
        limit: usize,
    ) -> Result<Vec<UnitOwnerBinding>> {
        if !(1..=128).contains(&limit) || through > self.snapshot().generation {
            return Err(WikiError::invalid("invalid unit owner change page"));
        }
        let seq = after.map_or(0, |x| x.modified_seq);
        let path = after.map_or("", |x| x.owner.as_str());
        let mut s = self.connection().prepare("SELECT owner,render_token,proof_version,modified_seq,tombstone,unit_count FROM unit_owners INDEXED BY unit_owner_changes WHERE policy=?1 AND modified_seq<=?2 AND modified_seq>?6 AND (modified_seq>?3 OR (modified_seq=?3 AND owner>?4)) ORDER BY modified_seq,owner LIMIT ?5").map_err(sql::sql_error)?;
        let mut rows = s
            .query(params![
                policy.as_str(),
                sql::integer(through)?,
                sql::integer(seq)?,
                path,
                limit as i64,
                sql::integer(since)?
            ])
            .map_err(sql::sql_error)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(sql::sql_error)? {
            out.push(self.decode_unit_owner(row, policy)?);
        }
        Ok(out)
    }
}

fn replace_owner(
    c: &Connection,
    policy: &RenderPolicyId,
    settings: &EmbeddingSettings,
    document: Option<&DocumentRow>,
    path: &VaultRelativePath,
    seq: u64,
    budget: &UnitBudget,
) -> Result<()> {
    let old_count: Option<i64> = c
        .query_row(
            "SELECT unit_count FROM unit_owners WHERE policy=?1 AND owner=?2",
            params![policy.as_str(), path.as_str()],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql::sql_error)?;
    let eligible = document.filter(|d| {
        d.eligibility == Eligibility::Current && matches!(d.kind, None | Some(RecordKind::Page))
    });
    if eligible.is_none() && old_count.is_none() {
        return Ok(());
    }
    let old_count = old_count.unwrap_or(0);
    let hash = document.map_or_else(|| Blake3Hash::digest([]), |d| d.hash.clone());
    let token = Blake3Hash::digest(crate::graph::packet::canonical_json(&(
        policy,
        path,
        &hash,
        eligible.is_some(),
    ))?);
    let units = match eligible {
        Some(d) => render_owner(d, settings, token.clone(), budget)?,
        None => Vec::new(),
    };
    // Descriptors contain no rendered UTF-8 or authentication fingerprint.
    c.execute(
        "DELETE FROM retrieval_units WHERE policy=?1 AND owner=?2",
        params![policy.as_str(), path.as_str()],
    )
    .map_err(sql::sql_error)?;
    for unit in &units {
        let descriptor = UnitDescriptor::from_rendered(policy, unit);
        c.execute(
            "INSERT INTO retrieval_units VALUES(?1,?2,?3,?4,?5)",
            params![
                policy.as_str(),
                path.as_str(),
                unit.unit_id.as_str(),
                unit.input_hash.as_str(),
                sql::json(&descriptor)?
            ],
        )
        .map_err(sql::sql_error)?;
    }
    c.execute("INSERT INTO unit_owners VALUES(?1,?2,?3,?4,?5,?5,?6,?7) ON CONFLICT(policy,owner) DO UPDATE SET source_hash=excluded.source_hash,render_token=excluded.render_token,proof_version=excluded.proof_version,modified_seq=excluded.modified_seq,tombstone=excluded.tombstone,unit_count=excluded.unit_count",params![policy.as_str(),path.as_str(),hash.as_str(),token.as_str(),sql::integer(seq)?,eligible.is_none(),units.len() as i64]).map_err(sql::sql_error)?;
    c.execute(
        "UPDATE unit_policies SET unit_count=unit_count+?2-?3 WHERE policy=?1",
        params![policy.as_str(), units.len() as i64, old_count],
    )
    .map_err(sql::sql_error)?;
    Ok(())
}

fn load_document(
    c: &Connection,
    owner: &VaultRelativePath,
    bytes: &mut usize,
) -> Result<Option<DocumentRow>> {
    let mut s = c
        .prepare(&format!(
            "SELECT {} FROM documents WHERE path=?1",
            normalized_schema::DOCUMENT_COLUMNS
        ))
        .map_err(sql::sql_error)?;
    let mut rows = s.query([owner.as_str()]).map_err(sql::sql_error)?;
    rows.next()
        .map_err(sql::sql_error)?
        .map(|row| {
            normalized_read::document(row, 0, |n| {
                *bytes = bytes
                    .checked_add(n)
                    .filter(|x| *x <= 64 * 1024 * 1024)
                    .ok_or_else(|| exhausted("unit maintenance document bytes exceeded"))?;
                Ok(())
            })
        })
        .transpose()
}

impl Catalog {
    /// Register/backfill one bounded keyset page. New publications maintain all
    /// registered policies, including a policy whose backfill is incomplete.
    pub(crate) fn prepare_unit_inventory_page(
        &self,
        writer: &WriterPermit,
        settings: &EmbeddingSettings,
        limit: usize,
    ) -> Result<UnitInventoryState> {
        writer.require_root(self.fs.root())?;
        settings.validate()?;
        if !(1..=128).contains(&limit) {
            return Err(WikiError::invalid(
                "unit backfill page requires 1..=128 owners",
            ));
        }
        let selection =
            selector::delta_selection(&self.fs, writer, &self.vault_id, Duration::from_secs(30))?;
        selector::ensure_delta_ready(&self.fs, writer, &selection, Duration::from_secs(30))?;
        let reader = self.cached_query_snapshot(QueryReadLimits::default())?;
        let snapshot = reader.snapshot().clone();
        reader.verify_operations(self)?;
        let sql_writer =
            selector::open_delta(&self.fs, writer, &selection, Duration::from_secs(30))?;
        let c = sql_writer.connection();
        sql::configure(c, 30_000, true)?;
        super::source_refresh::configure_delta(c)?;
        let tx = Transaction::new_unchecked(c, TransactionBehavior::Immediate)
            .map_err(sql::sql_error)?;
        if normalized_read::header(&tx, sql_writer.selection())?.snapshot != snapshot {
            return Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                "unit backfill publication changed",
            ));
        }
        tx.execute_batch(SCHEMA).map_err(sql::sql_error)?;
        let policy = RenderPolicyId::for_settings(&snapshot.parser_fingerprint, settings)?;
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM unit_policies WHERE policy=?1)",
                [policy.as_str()],
                |r| r.get(0),
            )
            .map_err(sql::sql_error)?;
        if !exists {
            let count: i64 = tx
                .query_row("SELECT count(*) FROM unit_policies", [], |r| r.get(0))
                .map_err(sql::sql_error)?;
            if count >= 4 {
                return Err(exhausted(
                    "unit inventory admits at most four render policies",
                ));
            }
            tx.execute("INSERT INTO unit_policies(policy,version,parser_hash,settings_json,complete,after_owner) VALUES(?1,1,?2,?3,0,NULL)",params![policy.as_str(),snapshot.parser_fingerprint.as_str(),sql::json(settings)?]).map_err(sql::sql_error)?;
        }
        let (complete, after): (bool, Option<String>) = tx
            .query_row(
                "SELECT complete,after_owner FROM unit_policies WHERE policy=?1",
                [policy.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(sql::sql_error)?;
        let mut cursor = after;
        let mut finished = complete;
        if !complete {
            let mut statement = tx.prepare("SELECT path FROM documents WHERE path COLLATE BINARY > ?1 AND eligibility='current' AND (kind IS NULL OR kind='page') ORDER BY path COLLATE BINARY LIMIT ?2").map_err(sql::sql_error)?;
            let mut rows = statement
                .query(params![cursor.as_deref().unwrap_or(""), limit as i64])
                .map_err(sql::sql_error)?;
            let mut paths = Vec::new();
            let mut path_bytes = 0usize;
            while let Some(row) = rows.next().map_err(sql::sql_error)? {
                let value = text(row, 0)?;
                path_bytes = path_bytes
                    .checked_add(value.len())
                    .filter(|n| *n <= 1024 * 1024)
                    .ok_or_else(|| exhausted("backfill path bytes exceeded"))?;
                paths.push(VaultRelativePath::new(value)?);
            }
            drop(rows);
            drop(statement);
            finished = paths.len() < limit;
            let budget = UnitBudget::new(UnitLimits::default())?;
            let mut bytes = 0;
            for path in &paths {
                // Already-maintained new owners need no duplicate backfill render.
                let present: bool = tx
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM unit_owners WHERE policy=?1 AND owner=?2)",
                        params![policy.as_str(), path.as_str()],
                        |r| r.get(0),
                    )
                    .map_err(sql::sql_error)?;
                if !present {
                    let document = load_document(&tx, path, &mut bytes)?;
                    replace_owner(
                        &tx,
                        &policy,
                        settings,
                        document.as_ref(),
                        path,
                        snapshot.generation,
                        &budget,
                    )?;
                }
            }
            if let Some(last) = paths.last() {
                cursor = Some(last.as_str().into());
            }
            tx.execute(
                "UPDATE unit_policies SET complete=?2,after_owner=?3 WHERE policy=?1",
                params![policy.as_str(), finished, cursor],
            )
            .map_err(sql::sql_error)?;
        }
        reader.verify_operations(self)?;
        let total: i64 = tx
            .query_row(
                "SELECT unit_count FROM unit_policies WHERE policy=?1",
                [policy.as_str()],
                |r| r.get(0),
            )
            .map_err(sql::sql_error)?;
        tx.commit().map_err(sql::sql_error)?;
        drop(reader);
        sql_writer.checkpoint_wal()?;
        Ok(UnitInventoryState {
            policy,
            complete: finished,
            after_owner: cursor.map(VaultRelativePath::new).transpose()?,
            through_seq: snapshot.generation,
            unit_count: usize::try_from(total).map_err(|_| corrupt("invalid inventory total"))?,
        })
    }

    /// Enroll the exact selected canonical dependency paths before publishing a
    /// readiness acknowledgment. A crash leaves extra invalidation hints only.
    pub(crate) fn record_unit_owner_dependencies(
        &self,
        writer: &WriterPermit,
        policy: &RenderPolicyId,
        snapshot: &ReadSnapshot,
        owners: &[(VaultRelativePath, Vec<ReadDependency>)],
    ) -> Result<()> {
        writer.require_root(self.fs.root())?;
        if owners.len() > 128 {
            return Err(exhausted("dependency enrollment owner cap exceeded"));
        }
        let reader = self.cached_query_snapshot(QueryReadLimits::default())?;
        reader.verify_operations(self)?;
        let selected =
            selector::acquire(&self.fs, &self.vault_id, Duration::from_secs(30), |_, s| {
                Ok(s.clone())
            })?
            .ok_or_else(|| corrupt("dependency enrollment selection absent"))?;
        let writer = selector::open_delta(
            &self.fs,
            writer,
            selected.selection(),
            Duration::from_secs(30),
        )?;
        sql::configure(writer.connection(), 30_000, true)?;
        super::source_refresh::configure_delta(writer.connection())?;
        let tx = Transaction::new_unchecked(writer.connection(), TransactionBehavior::Immediate)
            .map_err(sql::sql_error)?;
        if &normalized_read::header(&tx, writer.selection())?.snapshot != snapshot {
            return Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                "dependency enrollment publication changed",
            ));
        }
        let mut entries = 0usize;
        let mut bytes = 0usize;
        for (owner, guards) in owners {
            tx.execute(
                "DELETE FROM unit_owner_dependencies WHERE policy=?1 AND owner=?2",
                params![policy.as_str(), owner.as_str()],
            )
            .map_err(sql::sql_error)?;
            for guard in guards {
                entries += 1;
                bytes = bytes.saturating_add(owner.as_str().len() + guard.path.as_str().len());
                if entries > 65536 || bytes > 64 * 1024 * 1024 {
                    return Err(exhausted("unit dependency enrollment budget exceeded"));
                }
                tx.execute(
                    "INSERT OR IGNORE INTO unit_owner_dependencies VALUES(?1,?2,?3)",
                    params![policy.as_str(), owner.as_str(), guard.path.as_str()],
                )
                .map_err(sql::sql_error)?;
            }
        }
        reader.verify_operations(self)?;
        tx.commit().map_err(sql::sql_error)
    }
}

/// Changed publication inputs, captured before replacing the indexed rows.
pub(crate) struct AffectedUnits {
    owners: BTreeSet<(RenderPolicyId, VaultRelativePath)>,
    documents: BTreeSet<VaultRelativePath>,
}
fn bounded_previous(
    c: &Connection,
    query: &str,
    key: &str,
    bytes: &mut usize,
) -> Result<Option<String>> {
    let mut statement = c.prepare(query).map_err(sql::sql_error)?;
    let mut rows = statement.query([key]).map_err(sql::sql_error)?;
    let Some(row) = rows.next().map_err(sql::sql_error)? else {
        return Ok(None);
    };
    let value = text(row, 0)?;
    *bytes = bytes
        .checked_add(value.len())
        .filter(|n| *n <= 64 * 1024 * 1024)
        .ok_or_else(|| exhausted("unit invalidation comparison bytes exceeded"))?;
    if value.len() > 8 * 1024 * 1024 {
        return Err(exhausted("unit invalidation row bytes exceeded"));
    }
    Ok(Some(value.into()))
}
fn seed_record(
    c: &Connection,
    delta: &CatalogDelta,
    id: &RecordId,
    paths: &mut BTreeSet<VaultRelativePath>,
    bytes: &mut usize,
) -> Result<()> {
    if let Some(path) = bounded_previous(
        c,
        "SELECT path FROM records WHERE id=?1",
        id.as_str(),
        bytes,
    )? {
        paths.insert(VaultRelativePath::new(path)?);
    }
    if let Some(row) = delta.records.iter().find(|row| row.record.id() == id) {
        paths.insert(row.path.clone());
    }
    Ok(())
}
/// Compare actual expectations/facts, rather than treating the unchanged
/// witnesses included in operational publications as changed dependencies.
pub(crate) fn affected_before(c: &Connection, delta: &CatalogDelta) -> Result<AffectedUnits> {
    use super::eligibility_facts::EligibilityRole;
    let mut out = AffectedUnits {
        owners: BTreeSet::new(),
        documents: BTreeSet::new(),
    };
    if !present(c)? {
        return Ok(out);
    }
    let mut paths = BTreeSet::new();
    let mut bytes = 0usize;
    for dep in &delta.dependencies {
        let old: Option<Option<String>> = c
            .query_row(
                "SELECT expected_hash FROM dependencies WHERE path=?1",
                [dep.path.as_str()],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql::sql_error)?;
        let next = match &dep.expected {
            ExpectedState::Absent => None,
            ExpectedState::Hash(h) => Some(h.as_str()),
        };
        if old.as_ref().map(|x| x.as_deref()) != Some(next) {
            paths.insert(dep.path.clone());
        }
    }
    for row in &delta.records {
        let old = bounded_previous(
            c,
            "SELECT row_json FROM records WHERE id=?1",
            row.record.id().as_str(),
            &mut bytes,
        )?;
        if old.as_deref() != Some(sql::json(row)?.as_str()) {
            seed_record(c, delta, row.record.id(), &mut paths, &mut bytes)?;
        }
    }
    for doc in &delta.documents {
        match doc {
            DocumentMutation::Put { row } => {
                let old = load_document(c, &row.path, &mut bytes)?;
                if old.as_ref() != Some(row) {
                    out.documents.insert(row.path.clone());
                }
            }
            DocumentMutation::DeletePage { path, .. } => {
                out.documents.insert(path.clone());
            }
            DocumentMutation::MovePage { from, row, .. } => {
                out.documents.insert(from.clone());
                out.documents.insert(row.path.clone());
            }
            DocumentMutation::Metadata {
                path,
                eligibility,
                reasons,
            } => {
                let old = load_document(c, path, &mut bytes)?;
                if old
                    .as_ref()
                    .is_none_or(|d| d.eligibility != *eligibility || d.reasons != *reasons)
                {
                    out.documents.insert(path.clone());
                }
            }
        }
    }
    paths.extend(out.documents.iter().cloned());
    if let Some(facts) = &delta.facts {
        for mutation in &facts.records {
            let baseline = bounded_previous(
                c,
                "SELECT baseline_json FROM record_eligibility_facts WHERE record_id=?1",
                mutation.record_id.as_str(),
                &mut bytes,
            )?;
            let structural = bounded_previous(
                c,
                "SELECT structural_json FROM record_eligibility_facts WHERE record_id=?1",
                mutation.record_id.as_str(),
                &mut bytes,
            )?;
            let mut statement=c.prepare("SELECT path FROM record_direct_paths WHERE owner_id=?1 ORDER BY path LIMIT 4097").map_err(sql::sql_error)?;
            let mut rows = statement
                .query([mutation.record_id.as_str()])
                .map_err(sql::sql_error)?;
            let mut direct = BTreeSet::new();
            while let Some(row) = rows.next().map_err(sql::sql_error)? {
                let value = text(row, 0)?;
                bytes = bytes.saturating_add(value.len());
                if direct.len() == 4096 || bytes > 64 * 1024 * 1024 {
                    return Err(exhausted("unit invalidation fact comparison exceeded"));
                }
                direct.insert(VaultRelativePath::new(value)?);
            }
            if baseline.as_deref() != Some(sql::json(&mutation.fact.baseline)?.as_str())
                || structural.as_deref() != Some(sql::json(&mutation.fact.structural)?.as_str())
                || direct != mutation.fact.direct_paths
            {
                seed_record(c, delta, &mutation.record_id, &mut paths, &mut bytes)?;
            }
        }
        for edge in facts.edge_inserts.iter().chain(&facts.edge_deletes) {
            if edge.role == EligibilityRole::SourceInventory {
                continue;
            }
            seed_record(c, delta, &edge.owner_id, &mut paths, &mut bytes)?;
            let reverse = matches!(
                edge.role,
                EligibilityRole::DecisionInput
                    | EligibilityRole::DecisionOutput
                    | EligibilityRole::PolicySupersession
            ) || matches!(&edge.role,EligibilityRole::TypedReference{field} if matches!(field.as_str(),"wiki_supersedes_id"|"wiki_superseded_by_id"|"wiki_assertion_id"));
            if reverse {
                seed_record(c, delta, &edge.target_id, &mut paths, &mut bytes)?;
            }
        }
    }
    let mut visits = 0usize;
    let mut key_bytes = 0usize;
    for path in paths {
        key_bytes = key_bytes.saturating_add(path.as_str().len());
        if key_bytes > 8 * 1024 * 1024 {
            return Err(exhausted("unit invalidation lookup key bytes exceeded"));
        }
        let mut s=c.prepare("SELECT policy,owner FROM unit_owner_dependencies INDEXED BY unit_dependency_owners WHERE path=?1 LIMIT 4097").map_err(sql::sql_error)?;
        let mut rows = s.query([path.as_str()]).map_err(sql::sql_error)?;
        while let Some(row) = rows.next().map_err(sql::sql_error)? {
            visits += 1;
            key_bytes = key_bytes.saturating_add(text(row, 0)?.len() + text(row, 1)?.len());
            if visits > 65536 || key_bytes > 8 * 1024 * 1024 {
                return Err(exhausted("unit invalidation reverse lookup work exceeded"));
            }
            out.owners.insert((
                RenderPolicyId(Blake3Hash::new(text(row, 0)?)?),
                VaultRelativePath::new(text(row, 1)?)?,
            ));
            if out.owners.len() > 4096 {
                return Err(exhausted("unit proof invalidation owner cap exceeded"));
            }
        }
    }
    Ok(out)
}

/// Same publication transaction as document/fact replacement. Only affected
/// owners are rendered; dependency-only changes preserve exact unit identities.
pub(crate) fn apply_after(
    c: &Connection,
    delta: &CatalogDelta,
    affected: &AffectedUnits,
) -> Result<()> {
    if !present(c)? {
        return Ok(());
    }
    let seq: i64 = c
        .query_row(
            "SELECT CASE WHEN state='building' THEN epoch ELSE epoch+1 END FROM catalog_meta WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .map_err(sql::sql_error)?;
    if seq <= 0 {
        return Err(corrupt("invalid next unit publication sequence"));
    }
    let mut s = c
        .prepare("SELECT policy,settings_json FROM unit_policies ORDER BY policy LIMIT 5")
        .map_err(sql::sql_error)?;
    let mut rows = s.query([]).map_err(sql::sql_error)?;
    let mut policies = Vec::new();
    while let Some(row) = rows.next().map_err(sql::sql_error)? {
        if policies.len() == 4 {
            return Err(exhausted("unit policy registration cap exceeded"));
        }
        let settings_text = text(row, 1)?;
        if settings_text.len() > 256 * 1024 {
            return Err(exhausted("unit policy settings bytes exceeded"));
        }
        let settings: EmbeddingSettings = serde_json::from_str(settings_text)
            .map_err(|_| corrupt("invalid unit policy settings"))?;
        settings.validate()?;
        policies.push((RenderPolicyId(Blake3Hash::new(text(row, 0)?)?), settings));
    }
    drop(rows);
    drop(s);
    let paths = &affected.documents;
    let budget = UnitBudget::new(UnitLimits::default())?;
    let mut bytes = 0;
    for (policy, settings) in &policies {
        for path in paths {
            let doc = load_document(c, path, &mut bytes)?;
            replace_owner(c, policy, settings, doc.as_ref(), path, seq as u64, &budget)?;
        }
    }
    // Retired Page owners keep the ordinary tombstone notification but no
    // selected-proof enrollment or rendered unit data from the absent file.
    for document in &delta.documents {
        if let DocumentMutation::DeletePage { path, .. } = document {
            c.execute(
                "DELETE FROM unit_owner_dependencies WHERE owner=?1",
                [path.as_str()],
            )
            .map_err(sql::sql_error)?;
        }
    }
    for (policy, owner) in &affected.owners {
        if !paths.contains(owner) {
            c.execute("UPDATE unit_owners SET proof_version=?3,modified_seq=?3 WHERE policy=?1 AND owner=?2",params![policy.as_str(),owner.as_str(),seq]).map_err(sql::sql_error)?;
        }
    }
    Ok(())
}

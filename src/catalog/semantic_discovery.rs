//! Transactional selection of disposable authenticated graph generations.
use super::semantic_discovery_schema::SCHEMA;
use super::{
    normalized_read,
    query::QuerySnapshot,
    query_types::{QueryCatalog, QueryReadLimits},
    selector, sql, Catalog,
};
use crate::{
    domain::*,
    retrieval::{
        discovery::{
            audit::{AuditCursor, AuditScope},
            DiscoveryManifest, DiscoveryState, GraphKind, GraphRef, FORMAT, MAX_DELTA, MAX_LEVEL,
            MAX_NODE_METADATA, MAX_NODE_VECTOR_BYTES,
        },
        unit_inventory_types::{catalog_incarnation, RenderPolicyId},
    },
    vault::WriterPermit,
};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use std::time::Duration;
const JSON_LIMIT: usize = 64 * 1024;
fn corrupt(message: &str) -> WikiError {
    WikiError::new(ErrorCode::IndexCorrupt, message)
}
fn conflict(message: &str) -> WikiError {
    WikiError::new(ErrorCode::FreshnessConflict, message)
}
fn unavailable(message: &str) -> WikiError {
    WikiError::new(ErrorCode::CapabilityUnavailable, message)
}
fn present(c: &Connection) -> Result<bool> {
    c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='semantic_discovery_state')", [], |r| r.get(0)).map_err(sql::sql_error)
}
fn scalar_text<'a>(r: &'a rusqlite::Row<'_>, index: usize) -> Result<&'a str> {
    r.get_ref(index)
        .map_err(sql::sql_error)?
        .as_str()
        .map_err(|_| corrupt("discovery scalar is not text"))
}
fn count(value: i64) -> Result<usize> {
    usize::try_from(value).map_err(|_| corrupt("invalid discovery unit count"))
}
fn sequence(value: i64) -> Result<u64> {
    if value <= 0 {
        Err(corrupt("invalid discovery sequence"))
    } else {
        Ok(value as u64)
    }
}
fn graph(reference: &GraphRef, kind: GraphKind) -> Result<()> {
    if reference.format != FORMAT
        || reference.kind != kind
        || reference.root_bytes == 0
        || reference.root_bytes > MAX_NODE_METADATA
        || !(1..=8).contains(&reference.directory_height)
        || reference.dimensions == 0
        || reference.dimensions as usize > MAX_NODE_VECTOR_BYTES / 4
        || reference.max_level > MAX_LEVEL
        || (reference.node_count == 0) != reference.entry.is_none()
        || reference.entry.is_some_and(|id| id >= reference.node_count)
        || (reference.node_count == 0 && reference.max_level != 0)
        || (kind == GraphKind::Delta && reference.node_count as usize > MAX_DELTA)
    {
        return Err(corrupt("invalid discovery graph reference"));
    }
    Ok(())
}
fn validate_manifest(
    value: &DiscoveryManifest,
    space: &Blake3Hash,
    policy: &RenderPolicyId,
    incarnation: &Blake3Hash,
    required: u64,
    eligible: usize,
) -> Result<()> {
    if value.format != FORMAT
        || value.algorithm != crate::retrieval::discovery::algorithm_identity()
        || &value.space != space
        || &value.policy != policy
        || &value.incarnation != incarnation
        || value.through_seq == 0
        || value.through_seq > required
        || (value.base.is_some() && value.base_through_seq == 0)
        || value.base_through_seq > value.through_seq
        || value.coverage.eligible_current_units != value.coverage.compatible_paid_units
        || value.coverage.eligible_current_units != value.coverage.indexed_current_units
        || (value.through_seq == required && value.coverage.eligible_current_units != eligible)
    {
        return Err(corrupt(
            "discovery manifest identity, interval or coverage invalid",
        ));
    }
    if let Some(base) = &value.base {
        graph(base, GraphKind::Base)?;
    }
    if let Some(delta) = &value.delta {
        graph(delta, GraphKind::Delta)?;
    }
    if let (Some(base), Some(delta)) = (&value.base, &value.delta) {
        if base.dimensions != delta.dimensions {
            return Err(corrupt("discovery graph dimensions differ"));
        }
    }
    let nodes = value.base.as_ref().map_or(0, |g| u64::from(g.node_count))
        + value.delta.as_ref().map_or(0, |g| u64::from(g.node_count));
    if nodes < value.coverage.indexed_current_units as u64 {
        return Err(corrupt("discovery coverage exceeds graph membership"));
    }
    Ok(())
}
fn decode_state(
    row: &rusqlite::Row<'_>,
    space: &Blake3Hash,
    policy: &RenderPolicyId,
    incarnation: &Blake3Hash,
    generation: u64,
) -> Result<DiscoveryState> {
    if scalar_text(row, 0)? != incarnation.as_str() {
        return Err(conflict("discovery catalog incarnation changed"));
    }
    let required_seq = sequence(row.get(1).map_err(sql::sql_error)?)?;
    if required_seq > generation {
        return Err(corrupt("discovery sequence exceeds catalog publication"));
    }
    let eligible_current_units = count(row.get(2).map_err(sql::sql_error)?)?;
    let manifest = match row.get_ref(3).map_err(sql::sql_error)? {
        rusqlite::types::ValueRef::Null => None,
        value => {
            let json = value
                .as_str()
                .map_err(|_| corrupt("discovery manifest is not text"))?;
            if json.len() > JSON_LIMIT {
                return Err(corrupt("discovery manifest exceeds format bound"));
            }
            let manifest: DiscoveryManifest = serde_json::from_str(json)
                .map_err(|_| corrupt("invalid discovery manifest JSON"))?;
            validate_manifest(
                &manifest,
                space,
                policy,
                incarnation,
                required_seq,
                eligible_current_units,
            )?;
            Some(manifest)
        }
    };
    Ok(DiscoveryState {
        required_seq,
        eligible_current_units,
        manifest,
    })
}
impl QuerySnapshot {
    pub(crate) fn discovery_state(
        &self,
        space: &Blake3Hash,
        policy: &RenderPolicyId,
    ) -> Result<Option<DiscoveryState>> {
        let mut s = self.connection().prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='semantic_discovery_state'").map_err(sql::sql_error)?;
        let mut rows = s.query([]).map_err(sql::sql_error)?;
        let Some(row) = rows.next().map_err(sql::sql_error)? else {
            return Ok(None);
        };
        self.reserve_scalar_row(row, 1)?;
        drop(rows);
        drop(s);
        let incarnation = catalog_incarnation(self.vault_id(), self.snapshot())?;
        let mut s = self.connection().prepare("SELECT incarnation,required_seq,eligible_units,manifest_json FROM semantic_discovery_state WHERE space=?1 AND policy=?2").map_err(sql::sql_error)?;
        let mut rows = s
            .query(params![space.as_str(), policy.as_str()])
            .map_err(sql::sql_error)?;
        let Some(row) = rows.next().map_err(sql::sql_error)? else {
            return Ok(None);
        };
        self.reserve_scalar_row(row, 4)?;
        decode_state(row, space, policy, &incarnation, self.snapshot().generation).map(Some)
    }
}
fn read_state(
    c: &Connection,
    space: &Blake3Hash,
    policy: &RenderPolicyId,
    incarnation: &Blake3Hash,
    generation: u64,
) -> Result<Option<DiscoveryState>> {
    let mut s = c.prepare("SELECT incarnation,required_seq,eligible_units,manifest_json FROM semantic_discovery_state WHERE space=?1 AND policy=?2").map_err(sql::sql_error)?;
    let mut rows = s
        .query(params![space.as_str(), policy.as_str()])
        .map_err(sql::sql_error)?;
    rows.next()
        .map_err(sql::sql_error)?
        .map(|row| decode_state(row, space, policy, incarnation, generation))
        .transpose()
}
impl Catalog {
    fn discovery_write<T>(
        &self,
        permit: &WriterPermit,
        operation: impl FnOnce(&Transaction<'_>, &ReadSnapshot, &Blake3Hash) -> Result<T>,
    ) -> Result<T> {
        permit.require_root(self.fs.root())?;
        let selection =
            selector::delta_selection(&self.fs, permit, &self.vault_id, Duration::from_secs(30))?;
        selector::ensure_delta_ready(&self.fs, permit, &selection, Duration::from_secs(30))?;
        let reader = self.cached_query_snapshot(QueryReadLimits::default())?;
        let snapshot = reader.snapshot().clone();
        reader.verify_operations(self)?;
        let writer = selector::open_delta(&self.fs, permit, &selection, Duration::from_secs(30))?;
        sql::configure(writer.connection(), 30_000, true)?;
        super::source_refresh::configure_delta(writer.connection())?;
        let tx = Transaction::new_unchecked(writer.connection(), TransactionBehavior::Immediate)
            .map_err(sql::sql_error)?;
        if normalized_read::header(&tx, writer.selection())?.snapshot != snapshot {
            return Err(conflict("discovery publication changed"));
        }
        let incarnation = catalog_incarnation(&self.vault_id, &snapshot)?;
        let result = operation(&tx, &snapshot, &incarnation)?;
        reader.verify_operations(self)?;
        tx.commit().map_err(sql::sql_error)?;
        Ok(result)
    }
    pub(crate) fn register_discovery(
        &self,
        writer: &WriterPermit,
        space: &Blake3Hash,
        policy: &RenderPolicyId,
    ) -> Result<DiscoveryState> {
        self.discovery_write(writer, |tx, snapshot, incarnation| {
            let has_policy: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='unit_policies')", [], |row| row.get(0)).map_err(sql::sql_error)?;
            if !has_policy { return Err(unavailable("discovery requires unit inventory capability")); }
            let inventory: Option<(i64,i64)> = tx.query_row("SELECT complete,unit_count FROM unit_policies WHERE policy=?1", [policy.as_str()], |r| Ok((r.get(0)?,r.get(1)?))).optional().map_err(sql::sql_error)?;
            let Some((complete,total)) = inventory else { return Err(unavailable("discovery requires registered unit policy")); };
            if complete != 1 { return Err(unavailable("discovery requires complete unit inventory")); }
            let eligible = count(total)?;
            tx.execute_batch(SCHEMA).map_err(sql::sql_error)?;
            tx.execute("INSERT OR IGNORE INTO semantic_discovery_state(space,policy,incarnation,required_seq,eligible_units,manifest_json) VALUES(?1,?2,?3,?4,?5,NULL)",params![space.as_str(),policy.as_str(),incarnation.as_str(),sql::integer(snapshot.generation)?,sql::integer(eligible as u64)?]).map_err(sql::sql_error)?;
            read_state(tx, space, policy, incarnation, snapshot.generation)?.ok_or_else(|| corrupt("discovery registration absent"))
        })
    }
    pub(crate) fn publish_discovery(
        &self,
        writer: &WriterPermit,
        manifest: &DiscoveryManifest,
    ) -> Result<()> {
        let json = bounded_json(manifest)?;
        self.discovery_write(writer, |tx, snapshot, incarnation| {
            if !present(tx)? { return Err(unavailable("discovery registration absent")); }
            let state = read_state(tx, &manifest.space, &manifest.policy, incarnation, snapshot.generation)?.ok_or_else(|| unavailable("discovery registration absent"))?;
            if &manifest.incarnation != incarnation || manifest.through_seq != state.required_seq { return Err(conflict("discovery staged interval is no longer current")); }
            validate_manifest(manifest, &manifest.space, &manifest.policy, incarnation, state.required_seq, state.eligible_current_units)?;
            authorize_publication(tx,manifest,state.manifest.as_ref())?;
            tx.execute("UPDATE semantic_discovery_state SET manifest_json=?3 WHERE space=?1 AND policy=?2", params![manifest.space.as_str(),manifest.policy.as_str(),json]).map_err(sql::sql_error)?;
            tx.execute("DELETE FROM semantic_discovery_stages WHERE space=?1 AND policy=?2 AND incarnation=?3 AND required_seq=?4",params![manifest.space.as_str(),manifest.policy.as_str(),incarnation.as_str(),sql::integer(manifest.through_seq)?]).map_err(sql::sql_error)?;
            tx.execute("DELETE FROM semantic_discovery_audits WHERE space=?1 AND policy=?2 AND incarnation=?3 AND required_seq=?4",params![manifest.space.as_str(),manifest.policy.as_str(),incarnation.as_str(),sql::integer(manifest.through_seq)?]).map_err(sql::sql_error)?;
            Ok(())
        })
    }
    pub(crate) fn discovery_stage(
        &self,
        writer: &WriterPermit,
        space: &Blake3Hash,
        policy: &RenderPolicyId,
        incarnation: &Blake3Hash,
        required_seq: u64,
    ) -> Result<Option<serde_json::Value>> {
        self.discovery_write(writer, |tx,snapshot,current| {
            check_stage_state(tx, space,policy,incarnation,required_seq,current,snapshot.generation)?;
            let mut s = tx.prepare("SELECT incarnation,required_seq,stage_json FROM semantic_discovery_stages WHERE space=?1 AND policy=?2").map_err(sql::sql_error)?;
            let mut rows = s.query(params![space.as_str(),policy.as_str()]).map_err(sql::sql_error)?;
            let Some(row) = rows.next().map_err(sql::sql_error)? else { return Ok(None); };
            if scalar_text(row,0)? != incarnation.as_str() || sequence(row.get(1).map_err(sql::sql_error)?)? != required_seq { return Ok(None); }
            let json = scalar_text(row,2)?;
            if json.len() > JSON_LIMIT { return Err(corrupt("discovery stage exceeds format bound")); }
            serde_json::from_str(json).map(Some).map_err(|_| corrupt("invalid discovery stage JSON"))
        })
    }
    pub(crate) fn save_discovery_stage(
        &self,
        writer: &WriterPermit,
        space: &Blake3Hash,
        policy: &RenderPolicyId,
        incarnation: &Blake3Hash,
        required_seq: u64,
        value: &serde_json::Value,
    ) -> Result<()> {
        let json = bounded_json(value)?;
        self.discovery_write(writer, |tx,snapshot,current| {
            check_stage_state(tx,space,policy,incarnation,required_seq,current,snapshot.generation)?;
            protect_stage_save(tx,space,policy,incarnation,required_seq,value)?;
            tx.execute("INSERT INTO semantic_discovery_stages VALUES(?1,?2,?3,?4,?5) ON CONFLICT(space,policy) DO UPDATE SET incarnation=excluded.incarnation,required_seq=excluded.required_seq,stage_json=excluded.stage_json",params![space.as_str(),policy.as_str(),incarnation.as_str(),sql::integer(required_seq)?,json]).map_err(sql::sql_error)?;
            Ok(())
        })
    }
    pub(crate) fn begin_discovery_audit(
        &self,
        writer: &WriterPermit,
        scope: &AuditScope,
        input: &GraphRef,
        output: &GraphRef,
        expected: &Blake3Hash,
        stage: &serde_json::Value,
    ) -> Result<AuditCursor> {
        let cursor = AuditCursor::new(scope, input, output)?;
        let fields = AuditFields::new(scope, input, output, expected, &cursor)?;
        stage_matches(stage, &cursor)?;
        let stage_json = bounded_json(stage)?;
        self.discovery_write(writer,|tx,snapshot,current|{
            check_stage_state(tx,&scope.space,&scope.policy,&scope.incarnation,scope.required_seq,current,snapshot.generation)?;
            tx.execute_batch(SCHEMA).map_err(sql::sql_error)?;
            retire_stale_authority(tx,&scope.space,&scope.policy,&scope.incarnation,scope.required_seq)?;
            let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM semantic_discovery_audits WHERE space=?1 AND policy=?2)",params![scope.space.as_str(),scope.policy.as_str()],|row|row.get(0)).map_err(sql::sql_error)?;
            if exists{return Err(conflict("discovery audit authority already initialized for this interval"));}
            write_audit(tx,scope,&fields,None)?;write_stage(tx,scope,&stage_json)?;Ok(cursor)
        })
    }
    pub(crate) fn verify_discovery_audit(
        &self,
        writer: &WriterPermit,
        scope: &AuditScope,
        input: &GraphRef,
        output: &GraphRef,
        expected: &Blake3Hash,
        cursor: &AuditCursor,
    ) -> Result<()> {
        let fields = AuditFields::new(scope, input, output, expected, cursor)?;
        self.discovery_write(writer, |tx, snapshot, current| {
            check_stage_state(
                tx,
                &scope.space,
                &scope.policy,
                &scope.incarnation,
                scope.required_seq,
                current,
                snapshot.generation,
            )?;
            verify_audit(tx, scope, &fields)
        })
    }
    pub(crate) fn advance_discovery_audit(
        &self,
        writer: &WriterPermit,
        scope: &AuditScope,
        input: &GraphRef,
        output: &GraphRef,
        expected: &Blake3Hash,
        prior: &AuditCursor,
        next: &AuditCursor,
        stage: &serde_json::Value,
    ) -> Result<()> {
        let before = AuditFields::new(scope, input, output, expected, prior)?;
        let after = AuditFields::new(scope, input, output, expected, next)?;
        if next.next_node < prior.next_node
            || next.next_node - prior.next_node > 128
            || (next.next_node == prior.next_node
                && (next.next_node != scope.expected_nodes || before.cursor != after.cursor))
        {
            return Err(corrupt(
                "discovery audit advance is not a bounded forward page",
            ));
        }
        if next.next_node > prior.next_node {
            let previous: Option<(u64, VaultRelativePath, Blake3Hash)> =
                serde_json::from_str(&before.last)
                    .map_err(|_| corrupt("invalid previous audit ordering key"))?;
            let following: Option<(u64, VaultRelativePath, Blake3Hash)> =
                serde_json::from_str(&after.last)
                    .map_err(|_| corrupt("invalid next audit ordering key"))?;
            if following.is_none()
                || previous
                    .as_ref()
                    .zip(following.as_ref())
                    .is_some_and(|(before, after)| after <= before)
            {
                return Err(corrupt("discovery audit ordering did not advance"));
            }
        }
        stage_matches(stage, next)?;
        let json = bounded_json(stage)?;
        self.discovery_write(writer, |tx, snapshot, current| {
            check_stage_state(
                tx,
                &scope.space,
                &scope.policy,
                &scope.incarnation,
                scope.required_seq,
                current,
                snapshot.generation,
            )?;
            verify_audit(tx, scope, &before)?;
            write_audit(tx, scope, &after, Some(&before.cursor))?;
            write_stage(tx, scope, &json)
        })
    }
}
fn check_stage_state(
    c: &Connection,
    space: &Blake3Hash,
    policy: &RenderPolicyId,
    incarnation: &Blake3Hash,
    seq: u64,
    current: &Blake3Hash,
    generation: u64,
) -> Result<()> {
    if incarnation != current {
        return Err(conflict("discovery stage incarnation changed"));
    }
    if !present(c)? {
        return Err(unavailable("discovery registration absent"));
    }
    let state = read_state(c, space, policy, current, generation)?
        .ok_or_else(|| unavailable("discovery registration absent"))?;
    if state.required_seq != seq {
        return Err(conflict("discovery stage required interval changed"));
    }
    Ok(())
}
fn bounded_json(value: &impl serde::Serialize) -> Result<String> {
    struct Sink(Vec<u8>);
    impl std::io::Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.0.len().saturating_add(bytes.len()) > JSON_LIMIT {
                return Err(std::io::Error::other("discovery JSON bound"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut sink = Sink(Vec::new());
    serde_json::to_writer(&mut sink, value)
        .map_err(|_| WikiError::new(ErrorCode::BudgetExceeded, "discovery JSON exceeds64KiB"))?;
    String::from_utf8(sink.0).map_err(|_| corrupt("discovery JSON encoding invalid"))
}
pub(super) fn invalidate(c: &Connection, policy: &RenderPolicyId, seq: u64) -> Result<()> {
    if !present(c)? {
        return Ok(());
    }
    c.execute("UPDATE semantic_discovery_state SET required_seq=?2,eligible_units=(SELECT unit_count FROM unit_policies WHERE policy=?1) WHERE policy=?1",params![policy.as_str(),sql::integer(seq)?]).map_err(sql::sql_error)?;
    // Keep the old checkpoint until guarded cache maintenance replaces it.
    // Immutable graph objects and provider accounting are never deleted here.
    Ok(())
}
#[cfg(test)]
#[path = "semantic_discovery_tests.rs"]
mod tests;

struct AuditFields {
    identity: String,
    expected: String,
    next: i64,
    prefix: String,
    last: String,
    cursor: String,
}
fn canonical(value: &impl serde::Serialize) -> Result<String> {
    let bytes = crate::graph::packet::canonical_json(value)?;
    if bytes.len() > JSON_LIMIT {
        return Err(corrupt("discovery audit metadata exceeds64KiB"));
    }
    String::from_utf8(bytes).map_err(|_| corrupt("invalid audit metadata encoding"))
}
impl AuditFields {
    fn new(
        scope: &AuditScope,
        input: &GraphRef,
        output: &GraphRef,
        expected: &Blake3Hash,
        cursor: &AuditCursor,
    ) -> Result<Self> {
        cursor.validate(scope, input, output)?;
        let value = serde_json::to_value(cursor).map_err(|_| corrupt("audit cursor encoding"))?;
        Ok(Self {
            identity: canonical(&(scope, input, output))?,
            expected: expected.as_str().to_owned(),
            next: i64::from(cursor.next_node),
            prefix: cursor.digest.as_str().to_owned(),
            last: canonical(
                value
                    .get("last_key")
                    .ok_or_else(|| corrupt("audit cursor last key absent"))?,
            )?,
            cursor: canonical(cursor)?,
        })
    }
}
fn audit_table(c: &Connection) -> Result<bool> {
    c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='semantic_discovery_audits')",[],|row|row.get(0)).map_err(sql::sql_error)
}
fn stage_matches(stage: &serde_json::Value, cursor: &AuditCursor) -> Result<()> {
    if stage.get("audit")
        != Some(&serde_json::to_value(cursor).map_err(|_| corrupt("audit cursor encoding"))?)
    {
        return Err(corrupt("discovery stage audit differs from checked cursor"));
    }
    Ok(())
}
fn verify_audit(c: &Connection, scope: &AuditScope, fields: &AuditFields) -> Result<()> {
    if !audit_table(c)? {
        return Err(corrupt("discovery audit authority absent"));
    }
    let mut statement=c.prepare("SELECT incarnation,required_seq,identity_json,expected_digest,next_node,prefix_digest,last_key_json,cursor_json FROM semantic_discovery_audits WHERE space=?1 AND policy=?2").map_err(sql::sql_error)?;
    let mut rows = statement
        .query(params![scope.space.as_str(), scope.policy.as_str()])
        .map_err(sql::sql_error)?;
    let row = rows
        .next()
        .map_err(sql::sql_error)?
        .ok_or_else(|| corrupt("discovery audit authority absent"))?;
    for column in [0, 2, 3, 5, 6, 7] {
        if scalar_text(row, column)?.len() > JSON_LIMIT {
            return Err(corrupt("discovery audit authority exceeds format bound"));
        }
    }
    if scalar_text(row, 0)? != scope.incarnation.as_str()
        || row.get::<_, i64>(1).map_err(sql::sql_error)? != sql::integer(scope.required_seq)?
        || scalar_text(row, 2)? != fields.identity
        || scalar_text(row, 3)? != fields.expected
        || row.get::<_, i64>(4).map_err(sql::sql_error)? != fields.next
        || scalar_text(row, 5)? != fields.prefix
        || scalar_text(row, 6)? != fields.last
        || scalar_text(row, 7)? != fields.cursor
    {
        return Err(corrupt(
            "resumed audit cursor differs from catalog authority",
        ));
    }
    Ok(())
}
fn write_stage(c: &Connection, scope: &AuditScope, json: &str) -> Result<()> {
    c.execute("INSERT INTO semantic_discovery_stages VALUES(?1,?2,?3,?4,?5) ON CONFLICT(space,policy) DO UPDATE SET incarnation=excluded.incarnation,required_seq=excluded.required_seq,stage_json=excluded.stage_json",params![scope.space.as_str(),scope.policy.as_str(),scope.incarnation.as_str(),sql::integer(scope.required_seq)?,json]).map_err(sql::sql_error)?;
    Ok(())
}
fn write_audit(
    c: &Connection,
    scope: &AuditScope,
    fields: &AuditFields,
    prior: Option<&str>,
) -> Result<()> {
    let affected = if let Some(prior) = prior {
        c.execute("UPDATE semantic_discovery_audits SET next_node=?5,prefix_digest=?6,last_key_json=?7,cursor_json=?8 WHERE space=?1 AND policy=?2 AND incarnation=?3 AND required_seq=?4 AND cursor_json=?9",params![scope.space.as_str(),scope.policy.as_str(),scope.incarnation.as_str(),sql::integer(scope.required_seq)?,fields.next,fields.prefix,fields.last,fields.cursor,prior]).map_err(sql::sql_error)?
    } else {
        c.execute(
            "INSERT INTO semantic_discovery_audits VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                scope.space.as_str(),
                scope.policy.as_str(),
                scope.incarnation.as_str(),
                sql::integer(scope.required_seq)?,
                fields.identity,
                fields.expected,
                fields.next,
                fields.prefix,
                fields.last,
                fields.cursor
            ],
        )
        .map_err(sql::sql_error)?
    };
    if affected != 1 {
        return Err(conflict(
            "discovery audit authority compare-and-swap failed",
        ));
    }
    Ok(())
}
fn retire_stale_authority(
    c: &Connection,
    space: &Blake3Hash,
    policy: &RenderPolicyId,
    incarnation: &Blake3Hash,
    seq: u64,
) -> Result<()> {
    if audit_table(c)? {
        c.execute("DELETE FROM semantic_discovery_audits WHERE space=?1 AND policy=?2 AND (incarnation<>?3 OR required_seq<>?4)",params![space.as_str(),policy.as_str(),incarnation.as_str(),sql::integer(seq)?]).map_err(sql::sql_error)?;
    }
    Ok(())
}
fn protect_stage_save(
    c: &Connection,
    space: &Blake3Hash,
    policy: &RenderPolicyId,
    incarnation: &Blake3Hash,
    seq: u64,
    value: &serde_json::Value,
) -> Result<()> {
    retire_stale_authority(c, space, policy, incarnation, seq)?;
    let audit = value.get("audit").filter(|value| !value.is_null());
    if !audit_table(c)? {
        if audit.is_some() {
            return Err(corrupt(
                "ordinary stage save cannot initialize audit progress",
            ));
        }
        return Ok(());
    }
    let mut statement=c.prepare("SELECT next_node,prefix_digest,last_key_json,cursor_json FROM semantic_discovery_audits WHERE space=?1 AND policy=?2 AND incarnation=?3 AND required_seq=?4").map_err(sql::sql_error)?;
    let mut rows = statement
        .query(params![
            space.as_str(),
            policy.as_str(),
            incarnation.as_str(),
            sql::integer(seq)?
        ])
        .map_err(sql::sql_error)?;
    if let Some(row) = rows.next().map_err(sql::sql_error)? {
        let audit =
            audit.ok_or_else(|| corrupt("ordinary stage save cannot remove audit progress"))?;
        let stored = scalar_text(row, 3)?;
        if stored.len() > JSON_LIMIT {
            return Err(corrupt("stored audit cursor exceeds bound"));
        }
        if canonical(audit)? != stored
            || audit.get("next_node").and_then(serde_json::Value::as_i64)
                != Some(row.get(0).map_err(sql::sql_error)?)
            || audit.get("digest").and_then(serde_json::Value::as_str) != Some(scalar_text(row, 1)?)
            || canonical(
                audit
                    .get("last_key")
                    .ok_or_else(|| corrupt("audit last key missing"))?,
            )? != scalar_text(row, 2)?
        {
            return Err(corrupt(
                "ordinary stage save cannot alter authoritative audit progress",
            ));
        }
    } else if audit.is_some() {
        return Err(corrupt(
            "ordinary stage save cannot initialize audit progress",
        ));
    }
    Ok(())
}
fn retained_publication(
    c: &Connection,
    manifest: &DiscoveryManifest,
    old: Option<&DiscoveryManifest>,
) -> Result<()> {
    if manifest.base.is_none() && manifest.delta.is_none() {
        return Ok(());
    }
    if manifest.delta.is_none()
        && manifest.base.is_some()
        && old.is_some_and(|old| {
            old.base == manifest.base && old.base_through_seq == manifest.base_through_seq
        })
    {
        let changed:i64=c.query_row("SELECT COALESCE(sum(unit_count),0) FROM unit_owners INDEXED BY unit_owner_changes WHERE policy=?1 AND modified_seq>?2",params![manifest.policy.as_str(),sql::integer(manifest.base_through_seq)?],|row|row.get(0)).map_err(sql::sql_error)?;
        if changed == 0 {
            return Ok(());
        }
    }
    Err(corrupt(
        "new nonempty discovery graph requires completed audit authority",
    ))
}
fn authorize_publication(
    c: &Connection,
    manifest: &DiscoveryManifest,
    old: Option<&DiscoveryManifest>,
) -> Result<()> {
    if !audit_table(c)? {
        return retained_publication(c, manifest, old);
    }
    let mut statement=c.prepare("SELECT incarnation,required_seq,identity_json,expected_digest,cursor_json FROM semantic_discovery_audits WHERE space=?1 AND policy=?2").map_err(sql::sql_error)?;
    let mut rows = statement
        .query(params![manifest.space.as_str(), manifest.policy.as_str()])
        .map_err(sql::sql_error)?;
    let Some(row) = rows.next().map_err(sql::sql_error)? else {
        let stage: Option<String> = c
            .query_row(
                "SELECT stage_json FROM semantic_discovery_stages WHERE space=?1 AND policy=?2",
                params![manifest.space.as_str(), manifest.policy.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql::sql_error)?;
        if let Some(stage) = stage {
            if stage.len() > JSON_LIMIT {
                return Err(corrupt("publication stage exceeds bound"));
            }
            let value: serde_json::Value = serde_json::from_str(&stage)
                .map_err(|_| corrupt("publication stage JSON invalid"))?;
            if value.get("audit").is_some_and(|value| !value.is_null())
                || value
                    .get("output_root")
                    .is_some_and(|value| !value.is_null())
            {
                return Err(corrupt("publication output has no audit authority"));
            }
        }
        return retained_publication(c, manifest, old);
    };
    for column in [0, 2, 3, 4] {
        if scalar_text(row, column)?.len() > JSON_LIMIT {
            return Err(corrupt("publication audit metadata exceeds bound"));
        }
    }
    let (scope, input, output): (AuditScope, GraphRef, GraphRef) =
        serde_json::from_str(scalar_text(row, 2)?)
            .map_err(|_| corrupt("publication audit identity invalid"))?;
    let expected = Blake3Hash::new(scalar_text(row, 3)?)?;
    let cursor: AuditCursor = serde_json::from_str(scalar_text(row, 4)?)
        .map_err(|_| corrupt("publication audit cursor invalid"))?;
    if scope.space != manifest.space
        || scope.policy != manifest.policy
        || scope.incarnation != manifest.incarnation
        || scope.required_seq != manifest.through_seq
        || scalar_text(row, 0)? != manifest.incarnation.as_str()
        || row.get::<_, i64>(1).map_err(sql::sql_error)? != sql::integer(manifest.through_seq)?
        || match output.kind {
            GraphKind::Base => {
                manifest.base.as_ref() != Some(&output)
                    || manifest.delta.is_some()
                    || manifest.base_through_seq != manifest.through_seq
                    || scope.since_seq != 0
                    || scope.expected_nodes as usize != manifest.coverage.eligible_current_units
            }
            GraphKind::Delta => {
                let since = old.map_or(0, |old| old.base_through_seq);
                let changed:i64=c.query_row("SELECT COALESCE(sum(unit_count),0) FROM unit_owners INDEXED BY unit_owner_changes WHERE policy=?1 AND modified_seq>?2",params![manifest.policy.as_str(),sql::integer(since)?],|row|row.get(0)).map_err(sql::sql_error)?;
                manifest.delta.as_ref() != Some(&output)
                    || manifest.base.as_ref() != old.and_then(|old| old.base.as_ref())
                    || manifest.base_through_seq != since
                    || scope.since_seq != since
                    || i64::from(scope.expected_nodes) != changed
            }
            _ => true,
        }
    {
        return Err(corrupt(
            "publication graph differs from authoritative audit",
        ));
    }
    let fields = AuditFields::new(&scope, &input, &output, &expected, &cursor)?;
    verify_audit(c, &scope, &fields)?;
    cursor.finish(&expected)
}

//! Disposable SQLite vectors and fresh generation memberships, independent of canonical receipts.
type StoredVector = (i64, Vec<u8>, String, bool);
use super::{
    render::{RenderedUnit, TargetKind},
    spaces::SpaceSpec,
};
use crate::{
    domain::*,
    jobs::VectorCacheRef,
    vault::{VaultFs, WriterPermit},
};
use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};
use std::{cmp::Ordering, collections::BTreeMap, time::Duration};
const CACHE_PATH: &str = ".wiki/cache/embeddings.sqlite3";
fn sql(e: rusqlite::Error) -> WikiError {
    WikiError::new(ErrorCode::IndexCorrupt, format!("embedding cache: {e}"))
}
fn snapshot_key(snapshot: &ReadSnapshot) -> Result<String> {
    Ok(
        Blake3Hash::digest(crate::graph::packet::canonical_json(snapshot)?)
            .as_str()
            .into(),
    )
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpaceState {
    pub id: Blake3Hash,
    pub spec: SpaceSpec,
    pub actual_dimensions: Option<u32>,
    pub active: bool,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct Coverage {
    pub eligible_units: usize,
    pub available_units: usize,
    pub missing_units: usize,
    pub corrupt_units: usize,
    pub pending_units: usize,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DenseHit {
    pub unit_id: Blake3Hash,
    pub target: TargetKind,
    pub owner: VaultRelativePath,
    pub target_id: Option<RecordId>,
    pub source_span: Option<ByteSpan>,
    pub input_hash: Blake3Hash,
    pub score: f64,
}
impl Eq for DenseHit {}
impl PartialOrd for DenseHit {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
// The worst selected candidate is the heap maximum.
impl Ord for DenseHit {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .score
            .total_cmp(&self.score)
            .then(self.target.cmp(&other.target))
            .then(self.target_id.cmp(&other.target_id))
            .then(self.owner.cmp(&other.owner))
            .then(self.unit_id.cmp(&other.unit_id))
    }
}
pub struct DenseScan {
    pub hits: BTreeMap<TargetKind, Vec<DenseHit>>,
    pub coverage: Coverage,
    /// Number of distinct owners retained for each target, bounded by k.
    pub available_by_target: BTreeMap<TargetKind, usize>,
    /// At least one other owner was seen after the selected owner cap filled.
    pub owner_cap_reached_by_target: BTreeMap<TargetKind, bool>,
}
pub struct VectorStore {
    connection: Connection,
    writable: bool,
    #[cfg(test)]
    retained_budget: Option<std::rc::Rc<RetainedBudget>>,
}
pub fn normalize(values: &[f32]) -> Result<Vec<u8>> {
    if values.is_empty() || values.len() > 65536 || values.iter().any(|x| !x.is_finite()) {
        return Err(WikiError::new(
            ErrorCode::ProviderResponse,
            "invalid vector dimension or coordinate",
        ));
    }
    let norm = values
        .iter()
        .map(|x| f64::from(*x).powi(2))
        .sum::<f64>()
        .sqrt();
    if !norm.is_finite() || norm == 0.0 {
        return Err(WikiError::new(
            ErrorCode::ProviderResponse,
            "vector norm must be finite and nonzero",
        ));
    }
    let out = values
        .iter()
        .flat_map(|x| ((f64::from(*x) / norm) as f32).to_le_bytes())
        .collect::<Vec<_>>();
    decode(&out, values.len() as u32)?;
    Ok(out)
}
pub fn decode(bytes: &[u8], dimensions: u32) -> Result<Vec<f32>> {
    if dimensions == 0 || dimensions > 65536 || bytes.len() != dimensions as usize * 4 {
        return Err(WikiError::new(
            ErrorCode::IndexCorrupt,
            "vector blob dimension invalid",
        ));
    }
    let values = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect::<Vec<_>>();
    let norm = values.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>();
    if values.iter().any(|x| !x.is_finite())
        || !norm.is_finite()
        || norm == 0.0
        || (norm - 1.0).abs() > 1e-5
    {
        return Err(WikiError::new(
            ErrorCode::IndexCorrupt,
            "vector blob coordinate/norm invalid",
        ));
    }
    Ok(values)
}
pub fn cosine(a: &[f32], b: &[f32]) -> Result<f64> {
    if a.is_empty() || a.len() != b.len() || a.iter().chain(b).any(|v| !v.is_finite()) {
        return Err(WikiError::new(
            ErrorCode::IndexCorrupt,
            "cosine vectors invalid",
        ));
    }
    let dot = a
        .iter()
        .zip(b)
        .map(|(a, b)| f64::from(*a) * f64::from(*b))
        .sum::<f64>();
    let na = a.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>();
    let nb = b.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>();
    if na == 0.0 || nb == 0.0 || !na.is_finite() || !nb.is_finite() {
        return Err(WikiError::new(
            ErrorCode::IndexCorrupt,
            "cosine vector norm invalid",
        ));
    }
    Ok((dot / (na.sqrt() * nb.sqrt())).clamp(-1.0, 1.0))
}
impl VectorStore {
    pub fn open(fs: &VaultFs, writer: Option<&WriterPermit>) -> Result<Self> {
        let writable = writer.is_some();
        if let Some(w) = writer {
            w.require_root(fs.root())?;
            fs.ensure_directory(&VaultRelativePath::new(".wiki/cache")?, w)?;
        }
        for suffix in ["", "-wal", "-shm"] {
            fs.root()
                .resolve(&VaultRelativePath::new(format!("{CACHE_PATH}{suffix}"))?)?;
        }
        let path = fs.root().resolve(&VaultRelativePath::new(CACHE_PATH)?)?;
        if !path.exists() && !writable {
            return Err(WikiError::new(
                ErrorCode::OfflineUnavailable,
                "embedding cache absent; corpus/query coverage missing",
            ));
        }
        let connection = Connection::open_with_flags(
            path,
            if writable {
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE
            } else {
                OpenFlags::SQLITE_OPEN_READ_ONLY
            },
        )
        .map_err(sql)?;
        connection
            .busy_timeout(Duration::from_secs(1))
            .map_err(sql)?;
        if writable {
            connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS embedding_spaces(id TEXT PRIMARY KEY,spec TEXT NOT NULL,dimensions INTEGER,active INTEGER NOT NULL DEFAULT 0); CREATE UNIQUE INDEX IF NOT EXISTS one_active_embedding_space ON embedding_spaces(active) WHERE active=1; CREATE TABLE IF NOT EXISTS embedding_vectors(space TEXT NOT NULL,input TEXT NOT NULL,dimensions INTEGER NOT NULL,blob BLOB NOT NULL,hash TEXT NOT NULL,ready INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(space,input)); CREATE TABLE IF NOT EXISTS embedding_memberships(space TEXT NOT NULL,snapshot TEXT NOT NULL,unit TEXT NOT NULL,input TEXT NOT NULL,proof TEXT NOT NULL,metadata TEXT NOT NULL,PRIMARY KEY(space,snapshot,unit,input)); CREATE INDEX IF NOT EXISTS embedding_membership_scan ON embedding_memberships(space,snapshot,unit,input);").map_err(sql)?;
        }
        if writable {
            let mut statement = connection
                .prepare("PRAGMA table_info(embedding_vectors)")
                .map_err(sql)?;
            let fields = statement
                .query_map([], |r| r.get::<_, String>(1))
                .map_err(sql)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(sql)?;
            if !fields.iter().any(|name| name == "ready") {
                connection.execute_batch("ALTER TABLE embedding_vectors ADD COLUMN ready INTEGER NOT NULL DEFAULT 0;").map_err(sql)?;
            }
        }
        Ok(Self {
            connection,
            writable,
            #[cfg(test)]
            retained_budget: None,
        })
    }
    pub fn active(&self) -> Result<Option<SpaceState>> {
        self.space_where("active=1", None)
    }
    pub fn space(&self, id: &Blake3Hash) -> Result<Option<SpaceState>> {
        self.space_where("id=?1", Some(id.as_str()))
    }
    fn space_where(&self, condition: &str, id: Option<&str>) -> Result<Option<SpaceState>> {
        let query = format!(
            "SELECT CASE WHEN length(CAST(id AS BLOB))=71 THEN id ELSE '' END,CASE WHEN length(CAST(spec AS BLOB))<=1048576 THEN spec ELSE '' END,dimensions,active FROM embedding_spaces WHERE {condition}"
        );
        let mut st = self.connection.prepare(&query).map_err(sql)?;
        let mut rows = if let Some(id) = id {
            st.query([id])
        } else {
            st.query([])
        }
        .map_err(sql)?;
        let Some(row) = rows.next().map_err(sql)? else {
            return Ok(None);
        };
        let text = |column| -> Result<&str> {
            row.get_ref(column).map_err(sql)?.as_str().map_err(|_| {
                WikiError::new(ErrorCode::IndexCorrupt, "space scalar metadata invalid")
            })
        };
        let id = text(0)?;
        let spec = text(1)?;
        #[cfg(test)]
        if let Some(budget) = &self.retained_budget {
            budget.reserve_space(spec.len().saturating_add(id.len()).saturating_add(16))?;
        }
        if id.len() != 71 {
            return Err(WikiError::new(
                ErrorCode::IndexCorrupt,
                "space identity encoding length invalid",
            ));
        }
        let actual_dimensions: Option<u32> = row.get(2).map_err(sql)?;
        let active: bool = row.get(3).map_err(sql)?;
        let state = (|| -> Result<SpaceState> {
            let spec: SpaceSpec = serde_json::from_str(&spec).map_err(|_| {
                WikiError::new(ErrorCode::IndexCorrupt, "space configuration corrupt")
            })?;
            let id = Blake3Hash::new(id)?;
            if spec.id()? != id || actual_dimensions.is_some_and(|d| d == 0 || d > 65536) {
                return Err(WikiError::new(
                    ErrorCode::IndexCorrupt,
                    "space identity/dimension corrupt",
                ));
            }
            Ok(SpaceState {
                id,
                spec,
                actual_dimensions,
                active,
            })
        })()?;
        Ok(Some(state))
    }
    fn write_gate(&self) -> Result<()> {
        if !self.writable {
            Err(WikiError::new(
                ErrorCode::Usage,
                "embedding cache is read-only",
            ))
        } else {
            Ok(())
        }
    }
    pub fn prepare_space(&self, spec: &SpaceSpec) -> Result<Blake3Hash> {
        self.write_gate()?;
        let id = spec.id()?;
        let bytes =
            serde_json::to_string(spec).map_err(|_| WikiError::invalid("space encoding"))?;
        self.connection
            .execute(
                "INSERT OR IGNORE INTO embedding_spaces(id,spec,dimensions) VALUES(?1,?2,?3)",
                params![id.as_str(), bytes, spec.dimensions],
            )
            .map_err(sql)?;
        Ok(id)
    }
    fn raw_vector(&self, space: &Blake3Hash, input: &Blake3Hash) -> Result<Option<StoredVector>> {
        let mut statement = self.connection.prepare("SELECT dimensions,CASE WHEN dimensions BETWEEN 1 AND 65536 AND length(blob)=dimensions*4 THEN blob ELSE X'' END,CASE WHEN length(CAST(hash AS BLOB))=71 THEN hash ELSE '' END,ready FROM embedding_vectors WHERE space=?1 AND input=?2").map_err(sql)?;
        let mut rows = statement
            .query(params![space.as_str(), input.as_str()])
            .map_err(sql)?;
        let Some(row) = rows.next().map_err(sql)? else {
            #[cfg(test)]
            if let Some(budget) = &self.retained_budget {
                budget.reserve(0, 0, false)?;
            }
            return Ok(None);
        };
        let blob = row
            .get_ref(1)
            .map_err(sql)?
            .as_blob()
            .map_err(|_| WikiError::new(ErrorCode::IndexCorrupt, "vector blob type invalid"))?;
        let hash = row
            .get_ref(2)
            .map_err(sql)?
            .as_str()
            .map_err(|_| WikiError::new(ErrorCode::IndexCorrupt, "vector hash type invalid"))?;
        #[cfg(test)]
        if let Some(budget) = &self.retained_budget {
            budget.reserve(hash.len().saturating_add(16), blob.len(), false)?;
        }
        if hash.len() != 71 {
            return Err(WikiError::new(
                ErrorCode::IndexCorrupt,
                "vector hash encoding length invalid",
            ));
        }
        Ok(Some((
            row.get(0).map_err(sql)?,
            blob.to_vec(),
            hash.to_owned(),
            row.get(3).map_err(sql)?,
        )))
    }
    pub fn vector(&self, space: &Blake3Hash, input: &Blake3Hash) -> Result<Option<Vec<f32>>> {
        let Some((d, blob, hash, ready)) = self.raw_vector(space, input)? else {
            return Ok(None);
        };
        let Some(d) = u32::try_from(d).ok() else {
            return Ok(None);
        };
        let state = self
            .space(space)?
            .ok_or_else(|| WikiError::new(ErrorCode::IndexCorrupt, "vector space absent"))?;
        if !ready
            || state.actual_dimensions != Some(d)
            || Blake3Hash::digest(&blob).as_str() != hash
        {
            return Ok(None);
        }
        Ok(decode(&blob, d).ok())
    }
    /// Validate the complete batch before one transaction; only corpus fixes auto dimension.
    pub fn put_batch(
        &mut self,
        space: &Blake3Hash,
        inputs: &[Blake3Hash],
        values: &[Vec<f32>],
        corpus: bool,
        proof: &Blake3Hash,
    ) -> Result<Vec<VectorCacheRef>> {
        self.put_batch_with_ready(space, inputs, values, corpus, proof, true)
    }
    pub fn put_batch_staged(
        &mut self,
        space: &Blake3Hash,
        inputs: &[Blake3Hash],
        values: &[Vec<f32>],
        corpus: bool,
        proof: &Blake3Hash,
    ) -> Result<Vec<VectorCacheRef>> {
        self.put_batch_with_ready(space, inputs, values, corpus, proof, false)
    }
    fn put_batch_with_ready(
        &mut self,
        space: &Blake3Hash,
        inputs: &[Blake3Hash],
        values: &[Vec<f32>],
        corpus: bool,
        proof: &Blake3Hash,
        ready: bool,
    ) -> Result<Vec<VectorCacheRef>> {
        self.write_gate()?;
        if inputs.is_empty() || inputs.len() != values.len() {
            return Err(WikiError::new(
                ErrorCode::ProviderResponse,
                "vector batch count invalid",
            ));
        }
        let state = self
            .space(space)?
            .ok_or_else(|| WikiError::invalid("space missing"))?;
        let dimensions = values[0].len() as u32;
        if values.iter().any(|v| v.len() != dimensions as usize)
            || state.actual_dimensions.is_some_and(|d| d != dimensions)
            || (!corpus && state.actual_dimensions.is_none())
        {
            return Err(WikiError::new(
                ErrorCode::ProviderResponse,
                "batch dimensions do not match established corpus space",
            ));
        }
        let blobs = values
            .iter()
            .map(|v| normalize(v))
            .collect::<Result<Vec<_>>>()?;
        let tx = self.connection.transaction().map_err(sql)?;
        let existing = tx
            .query_row(
                "SELECT dimensions FROM embedding_spaces WHERE id=?1",
                [space.as_str()],
                |r| r.get::<_, Option<u32>>(0),
            )
            .map_err(sql)?;
        if existing.is_some_and(|d| d != dimensions) {
            return Err(WikiError::new(
                ErrorCode::ProviderResponse,
                "concurrent corpus dimension differs",
            ));
        }
        if corpus {
            tx.execute(
                "UPDATE embedding_spaces SET dimensions=?2 WHERE id=?1 AND dimensions IS NULL",
                params![space.as_str(), dimensions],
            )
            .map_err(sql)?;
        }
        let mut refs = Vec::new();
        for (input, blob) in inputs.iter().zip(blobs) {
            let hash = Blake3Hash::digest(&blob);
            tx.execute("INSERT INTO embedding_vectors(space,input,dimensions,blob,hash,ready) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(space,input) DO UPDATE SET dimensions=excluded.dimensions,blob=excluded.blob,hash=excluded.hash,ready=excluded.ready",params![space.as_str(),input.as_str(),dimensions,blob,hash.as_str(),ready]).map_err(sql)?;
            refs.push(VectorCacheRef {
                space_hash: space.clone(),
                input_hash: input.clone(),
                vector_hash: hash,
                dimensions,
                membership_fingerprint: proof.clone(),
            });
        }
        tx.commit().map_err(sql)?;
        Ok(refs)
    }
    pub fn confirm_refs(&mut self, refs: &[VectorCacheRef]) -> Result<()> {
        self.write_gate()?;
        for reference in refs {
            if !self.verify_ref(reference)? {
                return Err(WikiError::new(
                    ErrorCode::RecoveryRequired,
                    "paid cache reference unavailable before confirmation",
                ));
            }
        }
        let tx = self.connection.transaction().map_err(sql)?;
        for reference in refs {
            let changed=tx.execute("UPDATE embedding_vectors SET ready=1 WHERE space=?1 AND input=?2 AND hash=?3 AND dimensions=?4",params![reference.space_hash.as_str(),reference.input_hash.as_str(),reference.vector_hash.as_str(),reference.dimensions]).map_err(sql)?;
            if changed != 1 {
                return Err(WikiError::new(
                    ErrorCode::RecoveryRequired,
                    "paid cache binding changed before confirmation",
                ));
            }
        }
        tx.commit().map_err(sql)
    }
    pub fn verify_ref(&self, reference: &VectorCacheRef) -> Result<bool> {
        let row = self.raw_vector(&reference.space_hash, &reference.input_hash)?;
        let dimension = self
            .space(&reference.space_hash)?
            .and_then(|state| state.actual_dimensions);
        Ok(row.is_some_and(|(d, b, h, _ready)| {
            u32::try_from(d).ok() == Some(reference.dimensions)
                && dimension == Some(reference.dimensions)
                && h == reference.vector_hash.as_str()
                && Blake3Hash::digest(&b) == reference.vector_hash
                && decode(&b, reference.dimensions).is_ok()
        }))
    }
    pub fn coverage(&self, space: &Blake3Hash, units: &[RenderedUnit]) -> Result<Coverage> {
        let mut coverage = Coverage {
            eligible_units: units.len(),
            ..Default::default()
        };
        for u in units {
            if self.vector(space, &u.input_hash)?.is_some() {
                coverage.available_units += 1;
            } else {
                coverage.missing_units += 1;
                if let Some((d, blob, hash, ready)) = self.raw_vector(space, &u.input_hash)? {
                    let valid = u32::try_from(d).ok().is_some_and(|d| {
                        Blake3Hash::digest(&blob).as_str() == hash && decode(&blob, d).is_ok()
                    });
                    if !ready && valid {
                        coverage.pending_units += 1;
                    } else {
                        coverage.corrupt_units += 1;
                    }
                }
            }
        }
        Ok(coverage)
    }
    /// Caller holds vault writer and fresh canonical proof. Publish coverage and active
    /// pointer together in one disposable-cache transaction, never across databases.
    pub fn memberships(
        &mut self,
        space: &Blake3Hash,
        snapshot: &ReadSnapshot,
        units: &[RenderedUnit],
        activate: bool,
    ) -> Result<Coverage> {
        self.memberships_with_spec(space, snapshot, units, activate, None)
    }
    pub fn memberships_with_spec(
        &mut self,
        space: &Blake3Hash,
        snapshot: &ReadSnapshot,
        units: &[RenderedUnit],
        activate: bool,
        spec: Option<&SpaceSpec>,
    ) -> Result<Coverage> {
        self.write_gate()?;
        if let Some(spec) = spec
            && (spec.id()? != *space || !activate)
        {
            return Err(WikiError::invalid(
                "published representation policy binding differs",
            ));
        }
        let coverage = self.coverage(space, units)?;
        if activate && coverage.missing_units > 0 {
            return Err(WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "replacement coverage incomplete",
            ));
        }
        let key = snapshot_key(snapshot)?;
        let tx = self.connection.transaction().map_err(sql)?;
        tx.execute(
            "DELETE FROM embedding_memberships WHERE space=?1 AND snapshot=?2",
            params![space.as_str(), key],
        )
        .map_err(sql)?;
        for u in units {
            tx.execute("INSERT INTO embedding_memberships(space,snapshot,unit,input,proof,metadata) VALUES(?1,?2,?3,?4,?5,?6)",params![space.as_str(),key,u.unit_id.as_str(),u.input_hash.as_str(),u.dependency_fingerprint.as_str(),serde_json::to_string(u).map_err(|_|WikiError::invalid("unit encoding"))?]).map_err(sql)?;
        }
        if let Some(spec) = spec {
            tx.execute(
                "UPDATE embedding_spaces SET spec=?2 WHERE id=?1",
                params![
                    space.as_str(),
                    serde_json::to_string(spec)
                        .map_err(|_| WikiError::invalid("space encoding"))?
                ],
            )
            .map_err(sql)?;
        }
        if activate {
            tx.execute("UPDATE embedding_spaces SET active=0 WHERE active=1", [])
                .map_err(sql)?;
            tx.execute(
                "UPDATE embedding_spaces SET active=1 WHERE id=?1",
                [space.as_str()],
            )
            .map_err(sql)?;
        }
        // Only the latest explicit binding is retained; pinned readers may re-render
        // their own units and intersect by exact identity rather than trust this table.
        tx.execute(
            "DELETE FROM embedding_memberships WHERE space=?1 AND snapshot<>?2",
            params![space.as_str(), key],
        )
        .map_err(sql)?;
        tx.commit().map_err(sql)?;
        Ok(coverage)
    }
    /// Each SQL row carries at most one bounded vector. Selection memory is
    /// O(k) owners and two passages per owner for each target.
    /// `units` is the freshly rendered/filtered target universe; no stale member can score.
    pub fn exact(
        &self,
        space: &Blake3Hash,
        query: &[f32],
        units: &[RenderedUnit],
        target: TargetKind,
        k: usize,
    ) -> Result<Vec<DenseHit>> {
        Ok(self
            .exact_stream(
                space,
                query,
                || Ok(units.iter().cloned().map(Ok)),
                &[target],
                k,
                |_| Ok(true),
            )?
            .hits
            .remove(&target)
            .unwrap_or_default())
    }
    /// The factory must replay the same bounded corpus and eligibility view.
    /// Coverage is counted once; a second pass retrieves two units for each
    /// winning owner without storing metadata for every corpus owner.
    pub fn exact_stream<I, S, F>(
        &self,
        space: &Blake3Hash,
        query: &[f32],
        units: S,
        targets: &[TargetKind],
        k: usize,
        allowed: F,
    ) -> Result<DenseScan>
    where
        S: Fn() -> Result<I>,
        I: IntoIterator<Item = Result<RenderedUnit>>,
        F: Fn(&RenderedUnit) -> Result<bool>,
    {
        if k == 0 || k > 160 {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "exact candidate cap exceeds 160",
            ));
        }
        let state = self
            .space(space)?
            .ok_or_else(|| WikiError::new(ErrorCode::OfflineUnavailable, "space missing"))?;
        if state.actual_dimensions != Some(query.len() as u32) {
            return Err(WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "query/corpus dimensions differ",
            ));
        }
        // First pass identifies exact top owners using only their best unit.
        // A rejected owner may later win, so its earlier second-best unit
        // cannot be retained correctly with a one-pass O(k) selection.
        let mut owners: BTreeMap<TargetKind, BTreeMap<VaultRelativePath, DenseHit>> = targets
            .iter()
            .map(|target| (*target, BTreeMap::new()))
            .collect();
        let mut coverage = Coverage::default();
        let mut owner_cap_reached_by_target = BTreeMap::new();
        for unit in units()? {
            let unit = unit?;
            if !targets.contains(&unit.target) || !allowed(&unit)? {
                continue;
            }
            coverage.eligible_units += 1;
            let Some(vector) = self.vector(space, &unit.input_hash)? else {
                coverage.missing_units += 1;
                if let Some((d, blob, hash, ready)) = self.raw_vector(space, &unit.input_hash)? {
                    let valid = u32::try_from(d).ok().is_some_and(|d| {
                        Blake3Hash::digest(&blob).as_str() == hash && decode(&blob, d).is_ok()
                    });
                    if !ready && valid {
                        coverage.pending_units += 1;
                    } else {
                        coverage.corrupt_units += 1;
                    }
                }
                continue;
            };
            coverage.available_units += 1;
            let score = cosine(query, &vector)?;
            let hit = DenseHit {
                unit_id: unit.unit_id,
                target: unit.target,
                owner: unit.owner,
                target_id: unit.target_id,
                source_span: unit.source_span,
                input_hash: unit.input_hash,
                score,
            };
            let selected = owners.get_mut(&hit.target).expect("selected target");
            if let Some(best) = selected.get_mut(&hit.owner) {
                if hit < *best {
                    *best = hit;
                }
                continue;
            }
            if selected.len() == k {
                owner_cap_reached_by_target.insert(hit.target, true);
                let worst_owner = selected
                    .iter()
                    .max_by(|(_, a), (_, b)| a.cmp(b))
                    .map(|(owner, best)| (owner.clone(), best.clone()))
                    .expect("full owner selection");
                if hit >= worst_owner.1 {
                    continue;
                }
                selected.remove(&worst_owner.0);
            }
            selected.insert(hit.owner.clone(), hit);
        }
        let available_by_target = owners
            .iter()
            .map(|(target, selected)| (*target, selected.len()))
            .collect();
        // Replay only the final owners to recover their best two units even
        // when an early unit lost the moving k-owner cutoff. This costs one
        // more corpus traversal, retains O(k) unit metadata, and never treats
        // a candidate's arrival order as evidence of rank.
        let mut passages: BTreeMap<TargetKind, BTreeMap<VaultRelativePath, Vec<DenseHit>>> = owners
            .iter()
            .map(|(target, selected)| {
                (
                    *target,
                    selected
                        .keys()
                        .cloned()
                        .map(|owner| (owner, Vec::new()))
                        .collect(),
                )
            })
            .collect();
        for unit in units()? {
            let unit = unit?;
            if !passages
                .get(&unit.target)
                .is_some_and(|selected| selected.contains_key(&unit.owner))
                || !allowed(&unit)?
            {
                continue;
            }
            // Missing units were counted in the first pass and remain absent
            // from ranking. A vanished winning unit is caught by the final
            // best-hit comparison below.
            let Some(vector) = self.vector(space, &unit.input_hash)? else {
                continue;
            };
            let score = cosine(query, &vector)?;
            let selected = passages
                .get_mut(&unit.target)
                .expect("selected target")
                .get_mut(&unit.owner)
                .expect("selected owner");
            selected.push(DenseHit {
                unit_id: unit.unit_id,
                target: unit.target,
                owner: unit.owner,
                target_id: unit.target_id,
                source_span: unit.source_span,
                input_hash: unit.input_hash,
                score,
            });
            selected.sort();
            selected.truncate(2);
        }
        for (target, selected) in &owners {
            for (owner, best) in selected {
                if passages[target][owner].first() != Some(best) {
                    return Err(WikiError::new(
                        ErrorCode::FreshnessConflict,
                        "selected owner changed during exact replay",
                    ));
                }
            }
        }
        let hits = passages
            .into_iter()
            .map(|(target, selected)| {
                let mut groups = selected.into_values().collect::<Vec<_>>();
                groups.sort_by(|a, b| a[0].cmp(&b[0]));
                (target, groups.into_iter().flatten().collect())
            })
            .collect();
        Ok(DenseScan {
            hits,
            coverage,
            available_by_target,
            owner_cap_reached_by_target,
        })
    }
}

#[cfg(test)]
impl VectorStore {
    fn attach_retained_budget(&mut self, budget: std::rc::Rc<RetainedBudget>) -> Result<()> {
        let elapsed = u64::try_from(budget.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let remaining = budget.max_elapsed_ms.saturating_sub(elapsed);
        if remaining == 0 {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "retained read deadline exhausted",
            ));
        }
        self.connection
            .busy_timeout(Duration::from_millis(remaining.min(1000)))
            .map_err(sql)?;
        let observed = budget.steps.clone();
        let started = budget.started;
        let deadline = budget.max_elapsed_ms;
        self.connection
            .progress_handler(
                1,
                Some(move || {
                    let count = observed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    count >= 10_000_000 || started.elapsed().as_millis() >= u128::from(deadline)
                }),
            )
            .map_err(sql)?;
        self.retained_budget = Some(budget);
        Ok(())
    }
}

/// Diagnostic retained-cache scan only. This is not a fresh membership authority
/// and does not establish a scalable candidate index.
#[cfg(test)]
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct RetainedUsage {
    pub metadata_rows_decoded: usize,
    pub metadata_bytes_decoded: usize,
    pub vector_reads: usize,
    pub vector_bytes_scanned: usize,
    pub space_rows_decoded: usize,
    pub space_bytes_decoded: usize,
    pub sql_vm_steps: u64,
}
#[cfg(test)]
struct RetainedBudget {
    started: std::time::Instant,
    max_elapsed_ms: u64,
    usage: std::cell::RefCell<RetainedUsage>,
    steps: std::sync::Arc<std::sync::atomic::AtomicU64>,
}
#[cfg(test)]
impl RetainedBudget {
    fn reserve_space(&self, bytes: usize) -> Result<()> {
        let mut usage = self.usage.borrow_mut();
        if self.started.elapsed().as_millis() >= u128::from(self.max_elapsed_ms)
            || bytes > 1024 * 1024
            || usage
                .space_bytes_decoded
                .saturating_add(usage.metadata_bytes_decoded)
                .saturating_add(bytes)
                > 64 * 1024 * 1024
        {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "retained space metadata budget exhausted",
            ));
        }
        usage.space_rows_decoded += 1;
        usage.space_bytes_decoded += bytes;
        Ok(())
    }
    fn reserve(&self, metadata: usize, vectors: usize, membership: bool) -> Result<()> {
        let mut usage = self.usage.borrow_mut();
        if self.started.elapsed().as_millis() >= u128::from(self.max_elapsed_ms)
            || usage.metadata_rows_decoded + usize::from(membership) > 4096
            || metadata > 1024 * 1024
            || vectors > 65_536 * 4
            || usage
                .metadata_bytes_decoded
                .saturating_add(usage.space_bytes_decoded)
                .saturating_add(metadata)
                > 64 * 1024 * 1024
            || usage.vector_bytes_scanned.saturating_add(vectors) > 64 * 1024 * 1024
            || usage.vector_reads + usize::from(!membership) > 131_072
        {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "experimental retained embedding scan budget exhausted",
            ));
        }
        usage.metadata_rows_decoded += usize::from(membership);
        usage.metadata_bytes_decoded += metadata;
        usage.vector_reads += usize::from(!membership);
        usage.vector_bytes_scanned += vectors;
        Ok(())
    }
}
#[cfg(test)]
pub(crate) struct RetainedMembershipReader {
    store: VectorStore,
    pub state: SpaceState,
    /// Hash of the explicit legacy embedding snapshot; deliberately separate
    /// from the normalized catalog publication selected by the coordinator.
    pub snapshot_key: Blake3Hash,
}
#[cfg(test)]
impl RetainedMembershipReader {
    pub(crate) fn open(
        fs: &VaultFs,
        expected: Option<&SpaceState>,
        max_elapsed_ms: u64,
    ) -> Result<Self> {
        if max_elapsed_ms == 0
            || max_elapsed_ms > 5000
            || expected.is_some_and(|state| !state.active)
        {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "retained scan requires active space and finite five-second deadline",
            ));
        }
        let budget = std::rc::Rc::new(RetainedBudget {
            started: std::time::Instant::now(),
            max_elapsed_ms,
            usage: Default::default(),
            steps: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        });
        let observed_budget = budget.clone();
        let result = (|| -> Result<Self> {
            let mut store = VectorStore::open(fs, None)?;
            store.attach_retained_budget(budget)?;
            store.connection.execute_batch("PRAGMA mmap_size=0; PRAGMA cache_size=-8192; PRAGMA temp_store=FILE; BEGIN DEFERRED;").map_err(sql)?;
            let state = store.active()?.ok_or_else(|| {
                WikiError::new(
                    ErrorCode::OfflineUnavailable,
                    "active cached embedding space absent",
                )
            })?;
            if expected.is_some_and(|expected| {
                state.id != expected.id
                    || state.spec != expected.spec
                    || state.actual_dimensions != expected.actual_dimensions
            }) {
                return Err(WikiError::new(
                    ErrorCode::FreshnessConflict,
                    "active embedding space/settings changed before retained pin",
                ));
            }
            let mut statement = store.connection.prepare("SELECT CASE WHEN length(CAST(snapshot AS BLOB))=71 THEN snapshot ELSE '' END FROM embedding_memberships WHERE space=?1 GROUP BY snapshot LIMIT 2").map_err(sql)?;
            let mut rows = statement.query([state.id.as_str()]).map_err(sql)?;
            let row = rows.next().map_err(sql)?.ok_or_else(|| {
                WikiError::new(
                    ErrorCode::OfflineUnavailable,
                    "retained membership snapshot absent; explicit preparation required",
                )
            })?;
            let raw = row.get_ref(0).map_err(sql)?.as_str().map_err(|_| {
                WikiError::new(
                    ErrorCode::IndexCorrupt,
                    "retained snapshot identity invalid",
                )
            })?;
            store
                .retained_budget
                .as_ref()
                .expect("retained budget")
                .reserve_space(raw.len())?;
            if raw.len() != 71 {
                return Err(WikiError::new(
                    ErrorCode::IndexCorrupt,
                    "retained snapshot identity length invalid",
                ));
            }
            let snapshot_key = Blake3Hash::new(raw)?;
            if let Some(extra) = rows.next().map_err(sql)? {
                let bytes = extra
                    .get_ref(0)
                    .map_err(sql)?
                    .as_str()
                    .map_err(|_| {
                        WikiError::new(ErrorCode::IndexCorrupt, "retained snapshot scalar invalid")
                    })?
                    .len();
                store
                    .retained_budget
                    .as_ref()
                    .expect("retained budget")
                    .reserve_space(bytes)?;
                return Err(WikiError::new(
                    ErrorCode::IndexCorrupt,
                    "multiple explicit retained snapshots",
                ));
            }
            drop(rows);
            drop(statement);
            Ok(Self {
                store,
                state,
                snapshot_key,
            })
        })();
        result.map_err(|mut error| {
            let mut usage = observed_budget.usage.borrow().clone();
            usage.sql_vm_steps = observed_budget
                .steps
                .load(std::sync::atomic::Ordering::Relaxed);
            error.details["retained_work"] = serde_json::json!(usage);
            error
        })
    }
    pub(crate) fn store(&self) -> &VectorStore {
        &self.store
    }
    pub(crate) fn units(&self) -> RetainedMembershipIter<'_> {
        RetainedMembershipIter {
            reader: self,
            after: None,
            done: false,
        }
    }
    pub(crate) fn unit(&self, id: &Blake3Hash, input: &Blake3Hash) -> Result<Option<RenderedUnit>> {
        let mut iterator = self.units();
        iterator.after = Some((id.as_str().into(), String::new()));
        let unit = iterator.next().transpose()?;
        Ok(unit.filter(|unit| &unit.unit_id == id && &unit.input_hash == input))
    }
    pub(crate) fn usage(&self) -> RetainedUsage {
        let budget = self
            .store
            .retained_budget
            .as_ref()
            .expect("retained budget");
        let mut usage = budget.usage.borrow().clone();
        usage.sql_vm_steps = budget.steps.load(std::sync::atomic::Ordering::Relaxed);
        usage
    }
    pub(crate) fn recheck_active(&self, fs: &VaultFs) -> Result<()> {
        let mut fresh = VectorStore::open(fs, None)?;
        fresh.attach_retained_budget(
            self.store
                .retained_budget
                .as_ref()
                .expect("retained budget")
                .clone(),
        )?;
        let current = fresh.active()?.ok_or_else(|| {
            WikiError::new(
                ErrorCode::FreshnessConflict,
                "active embedding space disappeared",
            )
        })?;
        if current.id != self.state.id
            || current.spec != self.state.spec
            || current.actual_dimensions != self.state.actual_dimensions
        {
            return Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                "active embedding space/settings changed during selected query",
            ));
        }
        Ok(())
    }
}
#[cfg(test)]
pub(crate) struct RetainedMembershipIter<'a> {
    reader: &'a RetainedMembershipReader,
    after: Option<(String, String)>,
    done: bool,
}
#[cfg(test)]
impl Iterator for RetainedMembershipIter<'_> {
    type Item = Result<RenderedUnit>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let result = (|| -> Result<Option<RenderedUnit>> {
            let (unit_after, input_after) = self.after.clone().unwrap_or_default();
            let connection = &self.reader.store.connection;
            let mut statement = connection.prepare("SELECT CASE WHEN length(CAST(unit AS BLOB))=71 THEN unit ELSE '' END,CASE WHEN length(CAST(input AS BLOB))=71 THEN input ELSE '' END,CASE WHEN length(CAST(proof AS BLOB))=71 THEN proof ELSE '' END,length(CAST(metadata AS BLOB)),CASE WHEN length(CAST(metadata AS BLOB))<=1048576 THEN metadata ELSE NULL END FROM embedding_memberships WHERE space=?1 AND snapshot=?2 AND (unit,input)>(?3,?4) ORDER BY unit,input LIMIT 1").map_err(sql)?;
            let mut rows = statement
                .query(params![
                    self.reader.state.id.as_str(),
                    self.reader.snapshot_key.as_str(),
                    unit_after,
                    input_after
                ])
                .map_err(sql)?;
            let Some(row) = rows.next().map_err(sql)? else {
                return Ok(None);
            };
            let bytes = usize::try_from(row.get::<_, i64>(3).map_err(sql)?).map_err(|_| {
                WikiError::new(ErrorCode::IndexCorrupt, "retained metadata length invalid")
            })?;
            self.reader
                .store
                .retained_budget
                .as_ref()
                .expect("retained budget")
                .reserve(bytes.saturating_add(213), 0, true)?;
            let text = |column| -> Result<&str> {
                row.get_ref(column).map_err(sql)?.as_str().map_err(|_| {
                    WikiError::new(ErrorCode::IndexCorrupt, "retained scalar metadata invalid")
                })
            };
            let unit_id = text(0)?;
            let input_hash = text(1)?;
            let proof = text(2)?;
            if [unit_id, input_hash, proof]
                .iter()
                .any(|value| value.len() != 71)
            {
                return Err(WikiError::new(
                    ErrorCode::IndexCorrupt,
                    "retained membership hash length invalid",
                ));
            }
            let unit: RenderedUnit = serde_json::from_str(text(4)?).map_err(|_| {
                WikiError::new(ErrorCode::IndexCorrupt, "retained unit metadata invalid")
            })?;
            if unit.unit_id.as_str() != unit_id
                || unit.input_hash.as_str() != input_hash
                || unit.dependency_fingerprint.as_str() != proof
                || unit.utf8.len() > self.reader.state.spec.settings.max_input_bytes
                || Blake3Hash::digest(unit.utf8.as_bytes()) != unit.input_hash
            {
                return Err(WikiError::new(
                    ErrorCode::IndexCorrupt,
                    "retained unit columns/input hash/settings differ",
                ));
            }
            self.after = Some((unit_id.into(), input_hash.into()));
            Ok(Some(unit))
        })();
        match result {
            Ok(Some(unit)) => Some(Ok(unit)),
            Ok(None) => {
                self.done = true;
                None
            }
            Err(error) => {
                self.done = true;
                Some(Err(error))
            }
        }
    }
}

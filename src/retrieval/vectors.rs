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
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BinaryHeap},
    time::Duration,
};
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
    pub available_by_target: BTreeMap<TargetKind, usize>,
}
pub struct VectorStore {
    connection: Connection,
    writable: bool,
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
        })
    }
    pub fn active(&self) -> Result<Option<SpaceState>> {
        self.space_where("active=1", None)
    }
    pub fn space(&self, id: &Blake3Hash) -> Result<Option<SpaceState>> {
        self.space_where("id=?1", Some(id.as_str()))
    }
    fn space_where(&self, condition: &str, id: Option<&str>) -> Result<Option<SpaceState>> {
        let query =
            format!("SELECT id,spec,dimensions,active FROM embedding_spaces WHERE {condition}");
        let mut st = self.connection.prepare(&query).map_err(sql)?;
        let row = if let Some(id) = id {
            st.query_row([id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<u32>>(2)?,
                    r.get::<_, bool>(3)?,
                ))
            })
        } else {
            st.query_row([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<u32>>(2)?,
                    r.get::<_, bool>(3)?,
                ))
            })
        }
        .optional()
        .map_err(sql)?;
        row.map(|(id, spec, actual_dimensions, active)| {
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
        })
        .transpose()
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
        self.connection.query_row("SELECT dimensions,CASE WHEN dimensions BETWEEN 1 AND 65536 AND length(blob)=dimensions*4 THEN blob ELSE X'' END,hash,ready FROM embedding_vectors WHERE space=?1 AND input=?2",params![space.as_str(),input.as_str()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(sql)
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
    /// Each SQL row carries at most one bounded vector. Top-k heap memory is O(k).
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
                units.iter().cloned().map(Ok),
                &[target],
                k,
                |_| Ok(true),
            )?
            .hits
            .remove(&target)
            .unwrap_or_default())
    }
    pub fn exact_stream<I, F>(
        &self,
        space: &Blake3Hash,
        query: &[f32],
        units: I,
        targets: &[TargetKind],
        k: usize,
        mut allowed: F,
    ) -> Result<DenseScan>
    where
        I: IntoIterator<Item = Result<RenderedUnit>>,
        F: FnMut(&RenderedUnit) -> Result<bool>,
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
        let mut heaps: BTreeMap<TargetKind, BinaryHeap<DenseHit>> = targets
            .iter()
            .map(|target| (*target, BinaryHeap::new()))
            .collect();
        let mut coverage = Coverage::default();
        let mut available_by_target = BTreeMap::new();
        for unit in units {
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
            *available_by_target.entry(unit.target).or_insert(0) += 1;
            let score = cosine(query, &vector)?;
            let heap = heaps.get_mut(&unit.target).expect("selected target");
            heap.push(DenseHit {
                unit_id: unit.unit_id,
                target: unit.target,
                owner: unit.owner,
                target_id: unit.target_id,
                source_span: unit.source_span,
                input_hash: unit.input_hash,
                score,
            });
            if heap.len() > k {
                heap.pop();
            }
        }
        let hits = heaps
            .into_iter()
            .map(|(target, heap)| {
                let mut out = heap.into_vec();
                out.sort_by(|a, b| {
                    b.score
                        .total_cmp(&a.score)
                        .then(a.target_id.cmp(&b.target_id))
                        .then(a.owner.cmp(&b.owner))
                        .then(a.unit_id.cmp(&b.unit_id))
                });
                (target, out)
            })
            .collect();
        Ok(DenseScan {
            hits,
            coverage,
            available_by_target,
        })
    }
}

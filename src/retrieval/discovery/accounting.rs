//! Durable, non-refundable artifact reservations outside the disposable cache.
use super::admission::{DiscoveryAdmission, StorageGeometry, ACCOUNTING_ALLOWANCE};
use super::*;
use crate::vault::{DirectorySync, ExpectedState, VaultFs, WriterPermit};
use rusqlite::{params, Connection, OpenFlags};
use std::path::PathBuf;
use std::time::Duration;

const PATH: &str = ".wiki/semantic-accounting.sqlite3";
const MAX_DB: u64 = 16 * 1024 * 1024;
const ACCOUNT: &str = "CREATE TABLE resource_accounting(id INTEGER PRIMARY KEY CHECK(id=1),format INTEGER NOT NULL CHECK(format=1),aggregate_reserved INTEGER NOT NULL CHECK(aggregate_reserved>=0))";
const ATTEMPTS: &str = "CREATE TABLE resource_attempts(id TEXT PRIMARY KEY,plan_hash TEXT NOT NULL,admission_hash TEXT NOT NULL,reserved_bytes INTEGER NOT NULL CHECK(reserved_bytes>0),debit_bytes INTEGER NOT NULL CHECK(debit_bytes>=0 AND debit_bytes<=reserved_bytes),state TEXT NOT NULL CHECK(state='held'))";
fn db_error(e: rusqlite::Error) -> WikiError {
    if matches!(&e, rusqlite::Error::SqliteFailure(code, _) if code.code == rusqlite::ErrorCode::DiskFull || code.code == rusqlite::ErrorCode::OperationInterrupted)
    {
        exhausted("semantic accounting finite database budget exhausted")
    } else {
        corrupt(&format!("semantic accounting database: {e}"))
    }
}
fn integer(n: u64) -> Result<i64> {
    i64::try_from(n).map_err(|_| exhausted("semantic accounting integer overflow"))
}
fn sync(fs: &VaultFs, writer: &WriterPermit) -> Result<()> {
    if fs.sync_target(&VaultRelativePath::new(PATH)?, writer)? != DirectorySync::Supported {
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "semantic accounting requires durable directory sync",
        ));
    }
    Ok(())
}
fn regular(fs: &VaultFs, name: &str, max: u64) -> Result<bool> {
    let path = fs.root().resolve(&VaultRelativePath::new(name)?)?;
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_file() && m.len() <= max => Ok(true),
        Ok(_) => Err(corrupt(
            "semantic accounting path is not a bounded regular file",
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(corrupt(&format!("semantic accounting metadata: {e}"))),
    }
}
fn open(
    fs: &VaultFs,
    writer: &WriterPermit,
    admission: &DiscoveryAdmission,
    create: bool,
) -> Result<Connection> {
    writer.require_root(fs.root())?;
    admission.validate()?;
    let present = regular(fs, PATH, MAX_DB)?;
    let journal = regular(fs, &format!("{PATH}-journal"), 32 * 1024 * 1024)?;
    for suffix in ["-wal", "-shm"] {
        if regular(fs, &format!("{PATH}{suffix}"), MAX_DB)? {
            return Err(corrupt("semantic accounting has unexpected WAL sidecar"));
        }
    }
    if !present {
        let graph = fs
            .root()
            .resolve(&VaultRelativePath::new(".wiki/cache/semantic")?)?;
        let graph_present = match std::fs::symlink_metadata(graph) {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(corrupt(&format!("semantic cache metadata: {e}"))),
        };
        if !create || journal || graph_present {
            return Err(corrupt(
                "semantic accounting authority missing; existing graph artifacts cannot be adopted",
            ));
        }
        if fs.ensure_directory(&VaultRelativePath::new(".wiki")?, writer)?
            != DirectorySync::Supported
        {
            return Err(WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "semantic accounting requires directory sync",
            ));
        }
        let staged = fs.stage(&VaultRelativePath::new(PATH)?, &[], writer)?;
        if fs.replace(staged, &ExpectedState::Absent, writer)? != DirectorySync::Supported {
            return Err(WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "semantic accounting creation requires directory sync",
            ));
        }
    }
    let mut c = Connection::open_with_flags(
        fs.root().resolve(&VaultRelativePath::new(PATH)?)?,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(db_error)?;
    c.busy_timeout(Duration::from_millis(
        admission.limits.max_invocation_ms.min(30_000),
    ))
    .map_err(db_error)?;
    let started = std::time::Instant::now();
    let deadline = Duration::from_millis(admission.limits.max_invocation_ms.min(30_000));
    let mut steps = 0u32;
    c.progress_handler(
        1,
        Some(move || {
            steps += 1;
            steps > 10_000_000 || started.elapsed() >= deadline
        }),
    )
    .map_err(db_error)?;
    if present {
        let mode: String = c
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .map_err(db_error)?;
        if mode != "delete" {
            return Err(corrupt("semantic accounting journal mode mismatch"));
        }
    }
    c.execute_batch("PRAGMA page_size=4096; PRAGMA max_page_count=4096; PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA temp_store=MEMORY; PRAGMA cache_size=-2048;").map_err(db_error)?;
    for (name, expected) in [
        ("page_size", 4096i64),
        ("max_page_count", 4096),
        ("synchronous", 2),
        ("temp_store", 2),
        ("cache_size", -2048),
    ] {
        let actual: i64 = c
            .query_row(&format!("PRAGMA {name}"), [], |r| r.get(0))
            .map_err(db_error)?;
        if actual != expected {
            return Err(corrupt("semantic accounting SQLite limit mismatch"));
        }
    }
    c.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH, 16_384);
    c.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_SQL_LENGTH, 16_384);
    if !present {
        let tx = c
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db_error)?;
        tx.execute_batch(&format!("{ACCOUNT};{ATTEMPTS};"))
            .map_err(db_error)?;
        tx.execute(
            "INSERT INTO resource_accounting VALUES(1,1,?1)",
            [integer(ACCOUNTING_ALLOWANCE)?],
        )
        .map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        sync(fs, writer)?;
    }
    validate(&c)?;
    let aggregate: i64 = c
        .query_row(
            "SELECT aggregate_reserved FROM resource_accounting WHERE id=1",
            [],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    if aggregate as u64 > admission.limits.max_graph_cache_charge_bytes {
        return Err(exhausted(
            "semantic accounting existing holds exceed current aggregate policy",
        ));
    }
    Ok(c)
}
fn validate(c: &Connection) -> Result<()> {
    let mut stmt = c
        .prepare("SELECT type,name,sql FROM sqlite_master ORDER BY name")
        .map_err(db_error)?;
    let mut rows = stmt.query([]).map_err(db_error)?;
    let mut tables = 0;
    let mut index = false;
    while let Some(row) = rows.next().map_err(db_error)? {
        let kind: String = row.get(0).map_err(db_error)?;
        let name: String = row.get(1).map_err(db_error)?;
        let sql: Option<String> = row.get(2).map_err(db_error)?;
        if kind == "table"
            && ((name == "resource_accounting" && sql.as_deref() == Some(ACCOUNT))
                || (name == "resource_attempts" && sql.as_deref() == Some(ATTEMPTS)))
        {
            tables += 1;
        } else if kind == "index" && name == "sqlite_autoindex_resource_attempts_1" && sql.is_none()
        {
            index = true;
        } else {
            return Err(corrupt("semantic accounting schema mismatch"));
        }
    }
    if tables != 2 || !index {
        return Err(corrupt("semantic accounting schema incomplete"));
    }
    let (count, aggregate): (i64,i64) = c.query_row("SELECT count(*),max(aggregate_reserved) FROM resource_accounting WHERE id=1 AND format=1", [], |r| Ok((r.get(0)?,r.get(1)?))).map_err(db_error)?;
    let total: i64 = c
        .query_row("SELECT count(*) FROM resource_accounting", [], |r| r.get(0))
        .map_err(db_error)?;
    if count != 1 || total != 1 || aggregate < integer(ACCOUNTING_ALLOWANCE)? {
        return Err(corrupt("semantic accounting singleton invalid"));
    }
    let mut sum = ACCOUNTING_ALLOWANCE;
    let mut stmt = c.prepare("SELECT id,plan_hash,admission_hash,reserved_bytes,debit_bytes,state FROM resource_attempts").map_err(db_error)?;
    let mut rows = stmt.query([]).map_err(db_error)?;
    while let Some(r) = rows.next().map_err(db_error)? {
        for column in 0..3 {
            Blake3Hash::new(r.get::<_, String>(column).map_err(db_error)?)
                .map_err(|_| corrupt("semantic accounting hash invalid"))?;
        }
        let reserved: i64 = r.get(3).map_err(db_error)?;
        let debit: i64 = r.get(4).map_err(db_error)?;
        let state: String = r.get(5).map_err(db_error)?;
        if reserved <= 0 || debit < 0 || debit > reserved || state != "held" {
            return Err(corrupt("semantic accounting attempt invalid"));
        }
        sum = sum
            .checked_add(reserved as u64)
            .ok_or_else(|| corrupt("semantic accounting aggregate overflow"))?;
    }
    if sum != aggregate as u64 {
        return Err(corrupt(
            "semantic accounting aggregate does not match permanent holds",
        ));
    }
    Ok(())
}
fn attempt(c: &Connection, id: &Blake3Hash, admission: &DiscoveryAdmission) -> Result<(u64, u64)> {
    let row: (String,String,i64,i64,String) = c.query_row("SELECT plan_hash,admission_hash,reserved_bytes,debit_bytes,state FROM resource_attempts WHERE id=?1", [id.as_str()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).map_err(db_error)?;
    if row.0 != admission.plan_hash.as_str()
        || row.1 != admission.fingerprint()?.as_str()
        || row.2 != integer(admission.reserved_artifact_bytes)?
        || row.4 != "held"
        || row.3 < 0
        || row.3 > row.2
    {
        return Err(corrupt("semantic accounting attempt identity mismatch"));
    }
    Ok((row.2 as u64, row.3 as u64))
}
pub(crate) fn reserve(
    fs: &VaultFs,
    writer: &WriterPermit,
    admission: &DiscoveryAdmission,
) -> Result<Blake3Hash> {
    let mut c = open(fs, writer, admission, true)?;
    let tx = c
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(db_error)?;
    validate(&tx)?;
    let aggregate: i64 = tx
        .query_row(
            "SELECT aggregate_reserved FROM resource_accounting WHERE id=1",
            [],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    let next = (aggregate as u64)
        .checked_add(admission.reserved_artifact_bytes)
        .ok_or_else(|| exhausted("semantic accounting reservation overflow"))?;
    if next > admission.limits.max_graph_cache_charge_bytes {
        return Err(exhausted("semantic graph aggregate reservation exhausted"));
    }
    let id = Blake3Hash::digest(uuid::Uuid::now_v7().as_bytes());
    tx.execute(
        "INSERT INTO resource_attempts VALUES(?1,?2,?3,?4,0,'held')",
        params![
            id.as_str(),
            admission.plan_hash.as_str(),
            admission.fingerprint()?.as_str(),
            integer(admission.reserved_artifact_bytes)?
        ],
    )
    .map_err(db_error)?;
    tx.execute(
        "UPDATE resource_accounting SET aggregate_reserved=?1 WHERE id=1",
        [integer(next)?],
    )
    .map_err(db_error)?;
    tx.commit().map_err(db_error)?;
    sync(fs, writer)?;
    Ok(id)
}
pub(crate) fn verify(
    fs: &VaultFs,
    writer: &WriterPermit,
    id: &Blake3Hash,
    admission: &DiscoveryAdmission,
) -> Result<()> {
    let c = open(fs, writer, admission, false)?;
    attempt(&c, id, admission)?;
    Ok(())
}
pub(crate) fn debit(
    fs: &VaultFs,
    writer: &WriterPermit,
    id: &Blake3Hash,
    admission: &DiscoveryAdmission,
    bytes: u64,
) -> Result<ArtifactCredit> {
    let mut c = open(fs, writer, admission, false)?;
    let tx = c
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(db_error)?;
    validate(&tx)?;
    let (reserved, prior) = attempt(&tx, id, admission)?;
    let next = prior
        .checked_add(bytes)
        .filter(|v| *v <= reserved)
        .ok_or_else(|| exhausted("semantic attempt artifact reservation exhausted"))?;
    let changed = tx
        .execute(
            "UPDATE resource_attempts SET debit_bytes=?1 WHERE id=?2 AND debit_bytes=?3",
            params![integer(next)?, id.as_str(), integer(prior)?],
        )
        .map_err(db_error)?;
    if changed != 1 {
        return Err(corrupt("semantic accounting debit conflict"));
    }
    tx.commit().map_err(db_error)?;
    sync(fs, writer)?;
    Ok(ArtifactCredit {
        root: Some(fs.root().path().to_path_buf()),
        geometry: admission.geometry.clone(),
        remaining: bytes,
    })
}
/// A committed debit. Dropping this value never refunds durable accounting.
pub(crate) struct ArtifactCredit {
    root: Option<PathBuf>,
    geometry: StorageGeometry,
    remaining: u64,
}
impl ArtifactCredit {
    pub(crate) fn consume_object(&mut self, fs: &VaultFs, bytes: u64) -> Result<()> {
        if self
            .root
            .as_ref()
            .is_some_and(|root| root != fs.root().path())
        {
            return Err(corrupt("semantic artifact credit belongs to another vault"));
        }
        let charge = self.geometry.object_charge(bytes)?;
        self.remaining = self
            .remaining
            .checked_sub(charge)
            .ok_or_else(|| exhausted("semantic artifact credit exhausted before write"))?;
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self {
            root: None,
            geometry: StorageGeometry {
                allocation_unit_bytes: 4096,
            },
            remaining: u64::MAX,
        }
    }
}
#[cfg(test)]
#[path = "accounting_tests.rs"]
mod tests;

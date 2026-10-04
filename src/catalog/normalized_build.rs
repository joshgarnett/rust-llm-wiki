//! Bounded, unselected sibling construction. No corpus-sized SQL value is made.
use super::{file_types::BuildIdentity, normalized_schema, sql, types::*};
use crate::{
    domain::{
        Blake3Hash, Eligibility, ErrorCode, ReadSnapshot, RecordId, RecordKind, Result,
        VaultRelativePath, WikiError,
    },
    vault::{ExpectedState, VaultFs, WriterPermit},
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, limits::Limit, params};
use serde::Serialize;
use std::{
    fs::File,
    io::Write,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

fn link_key_kind(kind: super::link_facts::MatchKeyKind) -> &'static str {
    use super::link_facts::MatchKeyKind::*;
    match kind {
        Id => "id",
        Path => "path",
        Basename => "basename",
        Alias => "alias",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BuildCheckpoint {
    AfterHeader,
    AfterOrdinaryRow,
    AfterFtsRow,
    BeforeBatchCommit,
    AfterBatchCheckpoint,
    BeforeComplete,
    AfterComplete,
    BeforeSeal,
    BeforeSync,
    AfterSync,
}
pub(crate) trait BuildFault: Send + Sync {
    fn check(&self, step: BuildCheckpoint) -> Result<()>;
}

#[derive(Clone)]
pub(crate) struct BuildLimits {
    pub max_row_bytes: u64,
    pub max_batch_bytes: u64,
    pub max_batch_rows: u64,
    pub max_rows: u64,
    pub max_history_steps: u64,
    pub sqlite_cache_bytes: u64,
    pub max_database_bytes: u64,
    pub max_elapsed: Duration,
    pub fault: Option<Arc<dyn BuildFault>>,
}
impl Default for BuildLimits {
    fn default() -> Self {
        Self {
            max_row_bytes: 128 * 1024 * 1024,
            max_batch_bytes: 128 * 1024 * 1024,
            max_batch_rows: 256,
            max_rows: 10_000_000,
            max_history_steps: 10_000_000,
            sqlite_cache_bytes: 32 * 1024 * 1024,
            max_database_bytes: 32 * 1024 * 1024 * 1024,
            max_elapsed: Duration::from_secs(4 * 60 * 60),
            fault: None,
        }
    }
}
impl BuildLimits {
    fn validate(&self) -> Result<()> {
        if self.max_row_bytes == 0
            || self.max_row_bytes > self.max_batch_bytes
            || self.max_row_bytes > (i32::MAX as u64 - 65536)
            || self.max_batch_rows == 0
            || self.max_batch_rows > 256
            || self.max_rows == 0
            || self.max_history_steps == 0
            || self.sqlite_cache_bytes < 1024
            || self.sqlite_cache_bytes / 1024 > i32::MAX as u64
            || self.max_database_bytes < 4096
            || self.max_database_bytes / 4096 > 4_294_967_294
            || self.max_elapsed.is_zero()
            || Instant::now().checked_add(self.max_elapsed).is_none()
        {
            return Err(WikiError::new(
                ErrorCode::ConfigInvalid,
                "invalid normalized catalog build limits",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct BuildStats {
    pub identity_claims: u64,
    pub revision_owners: u64,
    pub history_steps: u64,
    pub documents: u64,
    pub graph_rows: u64,
    pub links: u64,
    pub link_facts: u64,
    pub registry_keys: u64,
    pub eligibility_facts: u64,
    pub direct_paths: u64,
    pub semantic_edges: u64,
    pub opposition_members: u64,
    pub assertion_navigation_keys: u64,
    pub records: u64,
    pub dependencies: u64,
    pub diagnostics: u64,
    pub admitted_bytes: u64,
    pub batches: u64,
    pub largest_row_bytes: u64,
    pub largest_batch_bytes: u64,
    pub database_bytes: u64,
    pub elapsed_ms: u64,
}

pub(crate) struct CompletedCatalog {
    pub identity: BuildIdentity,
    pub snapshot: ReadSnapshot,
    pub dependency_hash: Blake3Hash,
    pub stats: BuildStats,
    pub path: PathBuf,
}

pub(crate) struct NormalizedBuilder<'a> {
    fs: VaultFs,
    writer: &'a WriterPermit,
    identity: BuildIdentity,
    limits: BuildLimits,
    path: PathBuf,
    connection: Option<Connection>,
    started: Instant,
    batch_bytes: u64,
    batch_rows: u64,
    total_rows: u64,
    transaction_open: bool,
    poisoned: bool,
    stats: BuildStats,
    lookup_hash: blake3::Hasher,
}
impl<'a> NormalizedBuilder<'a> {
    pub fn begin(
        fs: &VaultFs,
        writer: &'a WriterPermit,
        identity: BuildIdentity,
        limits: BuildLimits,
    ) -> Result<Self> {
        writer.require_root(fs.root())?;
        fs.require_storage_ready()?;
        identity.selection.validate(&identity.selection.vault_id)?;
        limits.validate()?;
        let started = Instant::now();
        let io = fs.durable_io();
        for directory in [".wiki/cache", ".wiki/cache/catalogs"] {
            let relative = VaultRelativePath::new(directory)?;
            fs.root()
                .validate_portable_paths(std::slice::from_ref(&relative))?;
            let path = fs.root().resolve(&relative)?;
            match io.create_directory(&path) {
                Ok(()) => {
                    require_sync(
                        io.sync_directory(&path)
                            .map_err(|e| io_error("sync catalog directory", e))?,
                    )?;
                    require_sync(
                        io.sync_directory(path.parent().expect("managed directory parent"))
                            .map_err(|e| io_error("sync catalog directory parent", e))?,
                    )?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => {}
                Err(e) => return Err(io_error("create catalog directory", e)),
            }
        }
        let relative = VaultRelativePath::new(format!(
            ".wiki/cache/catalogs/{}.sqlite",
            identity.selection.file_id
        ))?;
        fs.root()
            .validate_portable_paths(std::slice::from_ref(&relative))?;
        let path = fs.root().resolve(&relative)?;
        // SQLite opens sidecars itself, so guard these names before handing it
        // the path. An unused primary filename must not adopt an old WAL.
        for suffix in ["-wal", "-shm", "-journal"] {
            let sidecar = VaultRelativePath::new(format!("{}{suffix}", relative.as_str()))?;
            fs.root()
                .validate_portable_paths(std::slice::from_ref(&sidecar))?;
            let sidecar = fs.root().resolve(&sidecar)?;
            match std::fs::symlink_metadata(sidecar) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(io_error("inspect new catalog sidecar", error)),
                Ok(_) => {
                    return Err(WikiError::new(
                        ErrorCode::IndexCorrupt,
                        "unused catalog identity has an existing SQLite sidecar",
                    ));
                }
            }
        }
        // Creation is exclusive even if an accidentally reused identity was supplied.
        let file = io
            .create_stage(&path)
            .map_err(|e| io_error("create catalog sibling", e))?;
        io.sync_file(&file)
            .map_err(|e| io_error("sync new catalog sibling", e))?;
        require_sync(
            io.sync_directory(path.parent().expect("catalog parent"))
                .map_err(|e| io_error("sync new catalog parent", e))?,
        )?;
        drop(file);
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(build_sql_error)?;
        connection
            .set_limit(
                Limit::SQLITE_LIMIT_LENGTH,
                (limits.max_row_bytes + 65536) as i32,
            )
            .map_err(build_sql_error)?;
        connection
            .set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, 256 * 1024)
            .map_err(build_sql_error)?;
        connection.execute_batch(&format!(
            "PRAGMA page_size=4096; PRAGMA max_page_count={}; PRAGMA mmap_size=0; PRAGMA cache_size=-{}; PRAGMA temp_store=FILE; PRAGMA wal_autocheckpoint=0;",
            limits.max_database_bytes / 4096, limits.sqlite_cache_bytes / 1024
        )).map_err(build_sql_error)?;
        sql::configure(&connection, 1000, true)?;
        let deadline = limits.max_elapsed;
        connection
            .progress_handler(1000, Some(move || started.elapsed() >= deadline))
            .map_err(build_sql_error)?;
        connection
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(build_sql_error)?;
        connection
            .execute_batch(normalized_schema::SCHEMA)
            .map_err(build_sql_error)?;
        let selection = &identity.selection;
        connection.execute("INSERT INTO catalog_meta(singleton,schema_version,vault_id,file_id,creation_epoch,creation_header_hash,epoch,state,origin_change_id,origin_manifest_hash,vector_cache_lost,vector_loss_unknown) VALUES(1,3,?1,?2,?3,?4,?3,'building',?5,?6,?7,?8)", params![
            selection.vault_id.as_str(), selection.file_id, sql::integer(selection.creation_epoch)?, selection.creation_header_hash.as_str(),
            identity.origin.as_ref().map(|v| v.change_id.as_str()), identity.origin.as_ref().map(|v| v.manifest_hash.as_str()),
            identity.vector_cache_lost, identity.vector_loss_unknown
        ]).map_err(build_sql_error)?;
        connection
            .execute_batch("COMMIT")
            .map_err(build_sql_error)?;
        checkpoint(&connection)?;
        let builder = Self {
            fs: fs.clone(),
            writer,
            identity,
            limits,
            path,
            connection: Some(connection),
            started,
            batch_bytes: 0,
            batch_rows: 0,
            total_rows: 0,
            transaction_open: false,
            poisoned: false,
            stats: BuildStats::default(),
            lookup_hash: blake3::Hasher::new(),
        };
        builder.step(BuildCheckpoint::AfterHeader)?;
        Ok(builder)
    }

    fn connection(&self) -> &Connection {
        self.connection.as_ref().expect("open builder connection")
    }
    fn guard(&self) -> Result<()> {
        if self.poisoned {
            return Err(WikiError::new(
                ErrorCode::IndexCorrupt,
                "normalized catalog builder is poisoned",
            ));
        }
        self.writer.require_root(self.fs.root())?;
        if self.started.elapsed() >= self.limits.max_elapsed {
            return Err(budget("normalized catalog build deadline exceeded"));
        }
        Ok(())
    }
    fn step(&self, step: BuildCheckpoint) -> Result<()> {
        self.guard()?;
        if let Some(fault) = &self.limits.fault {
            fault.check(step)?;
        }
        Ok(())
    }
    fn admit(&mut self, bytes: u64) -> Result<()> {
        self.guard()?;
        if bytes > self.limits.max_row_bytes || bytes > self.limits.max_batch_bytes {
            return Err(budget("normalized catalog row exceeds byte limit"));
        }
        if self.total_rows >= self.limits.max_rows {
            return Err(budget("normalized catalog exceeds row count limit"));
        }
        if self.batch_rows >= self.limits.max_batch_rows
            || self.batch_bytes > self.limits.max_batch_bytes - bytes
        {
            self.flush()?;
        }
        if !self.transaction_open {
            self.connection()
                .execute_batch("BEGIN IMMEDIATE")
                .map_err(build_sql_error)?;
            self.transaction_open = true;
        }
        self.total_rows += 1;
        self.batch_rows += 1;
        self.batch_bytes += bytes;
        self.stats.admitted_bytes = self
            .stats
            .admitted_bytes
            .checked_add(bytes)
            .ok_or_else(|| budget("catalog byte counter overflow"))?;
        self.stats.largest_row_bytes = self.stats.largest_row_bytes.max(bytes);
        self.stats.largest_batch_bytes = self.stats.largest_batch_bytes.max(self.batch_bytes);
        Ok(())
    }
    fn flush(&mut self) -> Result<()> {
        if !self.transaction_open {
            return Ok(());
        }
        self.step(BuildCheckpoint::BeforeBatchCommit)?;
        self.connection()
            .execute_batch("COMMIT")
            .map_err(build_sql_error)?;
        self.transaction_open = false;
        checkpoint(self.connection())?;
        self.stats.batches += 1;
        self.batch_bytes = 0;
        self.batch_rows = 0;
        self.step(BuildCheckpoint::AfterBatchCheckpoint)
    }
    fn failed(&mut self) {
        self.poisoned = true;
        if let Some(connection) = &self.connection
            && self.transaction_open
        {
            let _ = connection.execute_batch("ROLLBACK");
        }
        self.transaction_open = false;
    }

    fn insert_document(&mut self, row: &DocumentRow) -> Result<()> {
        self.guard()?;
        let aliases_size = counted_json(&row.aliases, self.limits.max_row_bytes)?;
        let tags_size = counted_json(&row.tags, self.limits.max_row_bytes)?;
        let reasons_size = counted_json(&row.reasons, self.limits.max_row_bytes)?;
        let aliases_text_size = joined_size(&row.aliases)?;
        let tags_text_size = joined_size(&row.tags)?;
        let ordinary = checked_sum(&[
            512,
            row.path.as_str().len() as u64,
            row.hash.as_str().len() as u64,
            row.record_id.as_ref().map_or(0, |v| v.as_str().len()) as u64,
            row.kind.map_or(0, |v| v.as_str().len()) as u64,
            row.source_id.as_ref().map_or(0, |v| v.as_str().len()) as u64,
            row.owner_revision.as_ref().map_or(0, |v| v.as_str().len()) as u64,
            eligibility(row.eligibility).len() as u64,
            row.title.len() as u64,
            row.headings.len() as u64,
            row.body.len() as u64,
            row.raw_text.len() as u64,
            aliases_size,
            tags_size,
            reasons_size,
            aliases_text_size,
            tags_text_size,
        ])?;
        let fts = checked_sum(&[
            row.title.len() as u64,
            aliases_text_size,
            row.headings.len() as u64,
            tags_text_size,
            row.body.len() as u64,
        ])?;
        self.admit(checked_sum(&[ordinary, fts])?)?;
        let aliases_json = sql::json(&row.aliases)?;
        let tags_json = sql::json(&row.tags)?;
        let reasons_json = sql::json(&row.reasons)?;
        let aliases_text = row.aliases.join(" ");
        let tags_text = row.tags.join(" ");
        self.connection().execute("INSERT INTO documents(path,record_id,kind,file_hash,title,aliases_json,aliases_text,headings,tags_json,tags_text,body,raw_text,source_id,owner_revision,eligibility,reasons_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)", params![
            row.path.as_str(),row.record_id.as_ref().map(|v|v.as_str()),row.kind.map(|v|v.as_str()),row.hash.as_str(),row.title,
            aliases_json,aliases_text,row.headings,tags_json,tags_text,row.body,row.raw_text,row.source_id.as_ref().map(|v|v.as_str()),
            row.owner_revision.as_ref().map(|v|v.as_str()),eligibility(row.eligibility),reasons_json
        ]).map_err(build_sql_error)?;
        let rowid = self.connection().last_insert_rowid();
        self.step(BuildCheckpoint::AfterOrdinaryRow)?;
        self.connection().execute("INSERT INTO documents_fts(rowid,title,aliases,headings,tags,body) VALUES(?1,?2,?3,?4,?5,?6)",
            params![rowid,row.title,aliases_text,row.headings,tags_text,row.body]).map_err(build_sql_error)?;
        self.step(BuildCheckpoint::AfterFtsRow)?;
        self.stats.documents += 1;
        Ok(())
    }
    fn insert_graph(&mut self, row: &GraphRow) -> Result<()> {
        self.guard()?;
        let aliases_size = counted_json(&row.aliases, self.limits.max_row_bytes)?;
        let aliases_text_size = joined_size(&row.aliases)?;
        let text = checked_sum(&[
            row.name.len() as u64,
            aliases_text_size,
            row.endpoints.len() as u64,
            row.predicate.len() as u64,
            row.qualifiers.len() as u64,
            row.description.len() as u64,
        ])?;
        self.admit(checked_sum(&[
            512,
            aliases_size,
            text,
            text,
            row.target_id.as_str().len() as u64,
            row.target_id.as_str().len() as u64,
            row.target_kind.as_str().len() as u64,
            row.target_kind.as_str().len() as u64,
        ])?)?;
        let aliases_json = sql::json(&row.aliases)?;
        let aliases_text = row.aliases.join(" ");
        self.connection().execute("INSERT INTO graph_rows(target_id,target_kind,name,aliases_json,aliases_text,endpoints,predicate,qualifiers,description) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![row.target_id.as_str(),row.target_kind.as_str(),row.name,aliases_json,aliases_text,row.endpoints,row.predicate,row.qualifiers,row.description]).map_err(build_sql_error)?;
        let rowid = self.connection().last_insert_rowid();
        self.step(BuildCheckpoint::AfterOrdinaryRow)?;
        self.connection().execute("INSERT INTO graph_fts(rowid,name,aliases,endpoints,predicate,qualifiers,description,target_kind,target_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![rowid,row.name,aliases_text,row.endpoints,row.predicate,row.qualifiers,row.description,row.target_kind.as_str(),row.target_id.as_str()]).map_err(build_sql_error)?;
        self.step(BuildCheckpoint::AfterFtsRow)?;
        self.stats.graph_rows += 1;
        Ok(())
    }
    fn insert_link(&mut self, row: &LinkRow) -> Result<()> {
        self.admit(checked_sum(&[
            512,
            row.from_path.as_str().len() as u64,
            row.target_id.as_ref().map_or(0, |v| v.as_str().len()) as u64,
            row.target_path.as_ref().map_or(0, |v| v.as_str().len()) as u64,
            row.resolution.len() as u64,
        ])?)?;
        self.connection().execute("INSERT INTO links(from_path,byte_start,target_id,target_path,resolution) VALUES(?1,?2,?3,?4,?5)",params![row.from_path.as_str(),sql::integer(row.byte_start)?,row.target_id.as_ref().map(|v|v.as_str()),row.target_path.as_ref().map(|v|v.as_str()),row.resolution]).map_err(build_sql_error)?;
        self.stats.links += 1;
        Ok(())
    }

    fn commit_lookup<T: Serialize>(&mut self, namespace: &str, row: &T) -> Result<()> {
        counted_json(row, self.limits.max_row_bytes)?;
        let encoded = sql::json(row)?;
        self.lookup_hash
            .update(&(namespace.len() as u64).to_le_bytes());
        self.lookup_hash.update(namespace.as_bytes());
        self.lookup_hash
            .update(&(encoded.len() as u64).to_le_bytes());
        self.lookup_hash.update(encoded.as_bytes());
        Ok(())
    }

    fn insert_eligibility_facts(
        &mut self,
        projection: &ValidationProjection,
        facts: &super::eligibility_facts::NormalizedEligibilityFacts,
    ) -> Result<()> {
        if facts.version != 2
            || facts.records.keys().ne(projection.records.keys())
            || projection
                .records
                .values()
                .any(|row| !row.dependencies.is_empty())
        {
            return Err(WikiError::new(
                ErrorCode::IndexCorrupt,
                "normalized proof layout or record scope is incomplete",
            ));
        }
        let expected: std::collections::BTreeMap<_, _> = projection
            .dependencies
            .iter()
            .map(|dependency| (&dependency.path, &dependency.expected))
            .collect();
        for (path, observed) in &facts.observed {
            self.guard()?;
            if expected.get(path).copied() != Some(observed) {
                return Err(WikiError::new(
                    ErrorCode::IndexCorrupt,
                    "normalized observation differs from complete dependency inventory",
                ));
            }
        }
        for (id, fact) in &facts.records {
            let record = &projection.records[id];
            let own = ExpectedState::Hash(record.hash.clone());
            if !fact.direct_paths.contains(&record.path)
                || expected.get(&record.path).copied() != Some(&own)
            {
                return Err(WikiError::new(
                    ErrorCode::IndexCorrupt,
                    "normalized record lacks its exact own canonical state",
                ));
            }
            if record.record.kind() == RecordKind::Assertion {
                let mut seen = std::collections::BTreeSet::new();
                for destination in super::scan::list(&record.record, "wiki_evidence") {
                    let resolution = match crate::records::links::untyped_lookup(&destination) {
                        crate::records::links::UntypedLookup::External => {
                            crate::records::LinkResolution::External
                        }
                        _ => crate::records::LinkResolution::Missing,
                    };
                    let fact = super::link_facts::untyped_fact(
                        &record.path,
                        0,
                        &destination,
                        &resolution,
                    )?;
                    for key in fact.keys {
                        self.guard()?;
                        if !seen.insert(key.clone()) {
                            continue;
                        }
                        self.admit(checked_sum(&[
                            256,
                            id.as_str().len() as u64,
                            key.value.len() as u64,
                        ])?)?;
                        self.connection()
                            .execute(
                                "INSERT INTO assertion_navigation_keys VALUES(?1,?2,?3)",
                                params![link_key_kind(key.kind), key.value, id.as_str()],
                            )
                            .map_err(build_sql_error)?;
                        self.commit_lookup("assertion_navigation_key", &(id, &key))?;
                        self.stats.assertion_navigation_keys += 1;
                    }
                }
            }
            if record.record.kind() == RecordKind::Assertion
                && record.record.string("wiki_status") == Some("accepted")
                && let Some((key, negated)) = super::eligibility::opposition_key(&record.record)
            {
                let bytes = counted_json(&key, self.limits.max_row_bytes)?;
                self.admit(checked_sum(&[256, id.as_str().len() as u64, bytes])?)?;
                self.connection().execute("INSERT INTO opposition_members(key_json,negated,assertion_id) VALUES(?1,?2,?3)", params![sql::json(&key)?,negated,id.as_str()]).map_err(build_sql_error)?;
                self.commit_lookup("opposition_member", &(&key, negated, id))?;
                self.stats.opposition_members += 1;
            }
            let bytes = counted_json(
                &(&fact.baseline, &fact.structural),
                self.limits.max_row_bytes,
            )?;
            self.admit(checked_sum(&[256, id.as_str().len() as u64, bytes])?)?;
            self.connection()
                .execute(
                    "INSERT INTO record_eligibility_facts(record_id,baseline_json,structural_json) VALUES(?1,?2,?3)",
                    params![id.as_str(), sql::json(&fact.baseline)?, sql::json(&fact.structural)?],
                )
                .map_err(build_sql_error)?;
            self.commit_lookup("baseline", &(id, &fact.baseline, &fact.structural))?;
            self.stats.eligibility_facts += 1;
            for path in &fact.direct_paths {
                if !expected.contains_key(path)
                    || (path != &record.path && crate::sources::revision::canonical_path(path))
                {
                    return Err(WikiError::new(
                        ErrorCode::IndexCorrupt,
                        "normalized direct path lacks state or captures another canonical note",
                    ));
                }
                self.admit(checked_sum(&[
                    256,
                    id.as_str().len() as u64,
                    path.as_str().len() as u64,
                ])?)?;
                self.connection()
                    .execute(
                        "INSERT INTO record_direct_paths(owner_id,path) VALUES(?1,?2)",
                        params![id.as_str(), path.as_str()],
                    )
                    .map_err(build_sql_error)?;
                self.commit_lookup("direct_path", &(id, path))?;
                self.stats.direct_paths += 1;
            }
        }
        for edge in &facts.edges {
            if !projection.records.contains_key(&edge.owner_id) {
                return Err(WikiError::new(
                    ErrorCode::IndexCorrupt,
                    "normalized semantic edge has unknown owner",
                ));
            }
            // Missing targets remain explicit facts for structurally invalid
            // records. They must not be silently removed during reconstruction.
            let bytes = counted_json(edge, self.limits.max_row_bytes)?;
            self.admit(checked_sum(&[256, bytes])?)?;
            self.connection()
                .execute(
                    "INSERT INTO semantic_edges(owner_id,target_id,role_json) VALUES(?1,?2,?3)",
                    params![
                        edge.owner_id.as_str(),
                        edge.target_id.as_str(),
                        sql::json(&edge.role)?
                    ],
                )
                .map_err(build_sql_error)?;
            self.commit_lookup("semantic_edge", edge)?;
            self.stats.semantic_edges += 1;
        }
        Ok(())
    }

    fn insert_link_fact(&mut self, row: &super::link_facts::OwnedLinkFact) -> Result<()> {
        let bytes = counted_json(row, self.limits.max_row_bytes)?;
        self.admit(checked_sum(&[256, bytes])?)?;
        self.connection().execute("INSERT INTO link_facts(from_path,byte_start,raw_destination,typed_id,typed_kind) VALUES(?1,?2,?3,?4,?5)",
            params![row.from_path.as_str(),sql::integer(row.byte_start)?,row.raw_destination,
                row.typed.as_ref().map(|target|target.id.as_str()),row.typed.as_ref().map(|target|target.expected_kind.as_str())]).map_err(build_sql_error)?;
        for key in &row.keys {
            self.admit(checked_sum(&[
                256,
                key.value.len() as u64,
                row.from_path.as_str().len() as u64,
            ])?)?;
            self.connection().execute("INSERT INTO link_match_keys(kind,value,from_path,byte_start) VALUES(?1,?2,?3,?4)",
                params![link_key_kind(key.kind),key.value,row.from_path.as_str(),sql::integer(row.byte_start)?]).map_err(build_sql_error)?;
        }
        self.commit_lookup("link_fact", row)?;
        self.stats.link_facts += 1;
        Ok(())
    }

    fn insert_registry_keys(
        &mut self,
        entry: &crate::records::RegistryEntry,
        keys: &[super::link_facts::MatchKey],
    ) -> Result<()> {
        for key in keys {
            self.admit(checked_sum(&[
                256,
                key.value.len() as u64,
                entry.id.as_str().len() as u64,
                entry.path.as_str().len() as u64,
            ])?)?;
            self.connection().execute("INSERT INTO registry_match_keys(kind,value,record_id,path) VALUES(?1,?2,?3,?4)",
                params![link_key_kind(key.kind),key.value,entry.id.as_str(),entry.path.as_str()]).map_err(build_sql_error)?;
            self.commit_lookup("registry_key", &(&entry.id, &entry.path, key))?;
            self.stats.registry_keys += 1;
        }
        Ok(())
    }

    fn reconstruct_revision_owners(&mut self) -> Result<Blake3Hash> {
        let engine = crate::changes::ChangeEngine::new(self.fs.clone())?;
        let writer = self.writer;
        let current = self
            .identity
            .origin
            .as_ref()
            .map(|origin| crate::changes::PreparedChange {
                change_id: origin.change_id.clone(),
                manifest_hash: origin.manifest_hash.clone(),
            });
        let mut fingerprint = blake3::Hasher::new();
        fingerprint.update(b"lwiki.revision-ownership.v1.build");
        let (started, max_elapsed, max_steps) = (
            self.started,
            self.limits.max_elapsed,
            self.limits.max_history_steps,
        );
        let mut history_steps = 0u64;
        let mut progress = || {
            if started.elapsed() > max_elapsed {
                return Err(budget("ownership reconstruction exceeds build deadline"));
            }
            history_steps = history_steps
                .checked_add(1)
                .filter(|n| *n <= max_steps)
                .ok_or_else(|| budget("ownership reconstruction exceeds history work ceiling"))?;
            Ok(())
        };
        engine.reconstruct_revision_owners(writer, current.as_ref(), &mut progress, &mut |row| {
            self.guard()?;
            let fields = [row.key.source_component.as_str(), row.key.revision_component.as_str(),
                row.change.change_id.as_str(), row.change.manifest_hash.as_str()];
            let mut bytes = 256u64;
            for field in fields {
                bytes = checked_sum(&[bytes, field.len() as u64])?;
            }
            self.admit(bytes)?;
            let inserted = self.connection().execute(
                "INSERT INTO revision_tree_owners(source_component,revision_component,change_id,manifest_hash) VALUES(?1,?2,?3,?4) ON CONFLICT(source_component,revision_component) DO NOTHING",
                params![fields[0], fields[1], fields[2], fields[3]],
            ).map_err(build_sql_error)?;
            if inserted == 0 {
                let same: bool = self.connection().query_row(
                    "SELECT change_id=?3 AND manifest_hash=?4 FROM revision_tree_owners WHERE source_component=?1 AND revision_component=?2",
                    params![fields[0], fields[1], fields[2], fields[3]], |row| row.get(0),
                ).map_err(build_sql_error)?;
                if !same {
                    return Err(WikiError::new(ErrorCode::ContentConflict,
                        "retained changes claim the same immutable revision tree"));
                }
                return Ok(());
            }
            for field in fields {
                fingerprint.update(&(field.len() as u64).to_le_bytes());
                fingerprint.update(field.as_bytes());
            }
            self.stats.revision_owners += 1;
            Ok(())
        })?;
        self.stats.history_steps = history_steps;
        // This flag is written only after the complete retained-history stream
        // succeeds. An empty table alone never authorizes bounded writes.
        self.connection().execute(
            "UPDATE catalog_meta SET revision_ownership_version=1 WHERE singleton=1 AND state='building'",
            [],
        ).map_err(build_sql_error)?;
        Blake3Hash::new(format!("blake3:{}", fingerprint.finalize().to_hex()))
    }

    fn insert_refresh_lookups(&mut self, projection: &ValidationProjection) -> Result<()> {
        // Rebuild work may inspect complete metadata once. Refresh queries use
        // these source-local indexes instead of reading historical payloads.
        for source in projection.records.values().filter(|row| {
            row.record.kind() == RecordKind::Source && row.eligibility != Eligibility::Invalid
        }) {
            let revisions = source
                .record
                .field("wiki_revisions")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| {
                    WikiError::new(
                        ErrorCode::IndexCorrupt,
                        "source revision list missing from validated projection",
                    )
                })?;
            for (ordinal, value) in revisions.iter().enumerate() {
                let revision_id = RecordId::new(value.as_str().ok_or_else(|| {
                    WikiError::new(
                        ErrorCode::IndexCorrupt,
                        "source revision identity is not text",
                    )
                })?)?;
                let row = projection
                    .records
                    .get(&revision_id)
                    .filter(|row| {
                        row.record.kind() == RecordKind::Revision
                            && row.record.string("wiki_source_id")
                                == Some(source.record.id().as_str())
                    })
                    .ok_or_else(|| {
                        WikiError::new(
                            ErrorCode::IndexCorrupt,
                            "validated retained revision identity is missing or foreign",
                        )
                    })?;
                let required = |field| {
                    row.record.string(field).ok_or_else(|| {
                        WikiError::new(
                            ErrorCode::IndexCorrupt,
                            format!("validated revision lacks {field}"),
                        )
                    })
                };
                let original = required("wiki_original_hash")?;
                let content = row.record.string("wiki_content_hash");
                let fingerprint = required("wiki_extractor_fingerprint")?;
                let status = required("wiki_extraction_status")?;
                self.admit(checked_sum(&[
                    256,
                    source.record.id().as_str().len() as u64,
                    revision_id.as_str().len() as u64,
                    original.len() as u64,
                    content.map_or(0, str::len) as u64,
                    fingerprint.len() as u64,
                    status.len() as u64,
                ])?)?;
                self.connection().execute("INSERT INTO source_revision_identity(source_id,revision_id,retained_ordinal,original_hash,content_hash,extractor_fingerprint,extraction_status) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![source.record.id().as_str(),revision_id.as_str(),sql::integer(ordinal as u64)?,original,content,fingerprint,status]).map_err(build_sql_error)?;
            }
        }
        for row in projection
            .records
            .values()
            .filter(|row| row.record.kind() == RecordKind::Evidence)
        {
            let source = row.record.string("wiki_source_id").ok_or_else(|| {
                WikiError::new(ErrorCode::IndexCorrupt, "evidence lacks source identity")
            })?;
            let assertion = row.record.string("wiki_assertion_id").ok_or_else(|| {
                WikiError::new(ErrorCode::IndexCorrupt, "evidence lacks assertion identity")
            })?;
            self.admit(checked_sum(&[
                256,
                source.len() as u64,
                row.record.id().as_str().len() as u64,
                assertion.len() as u64,
            ])?)?;
            self.connection().execute("INSERT INTO source_evidence(source_id,evidence_id,assertion_id) VALUES(?1,?2,?3)",params![source,row.record.id().as_str(),assertion]).map_err(build_sql_error)?;
        }
        Ok(())
    }

    pub fn finish(self, projection: &ValidationProjection) -> Result<CompletedCatalog> {
        self.finish_with_facts(projection, None)
    }

    pub fn finish_normalized(
        self,
        projection: &NormalizedValidationProjection,
    ) -> Result<CompletedCatalog> {
        self.finish_with_facts(&projection.validation, Some(&projection.facts))
    }

    fn finish_with_facts(
        mut self,
        projection: &ValidationProjection,
        facts: Option<&super::eligibility_facts::NormalizedEligibilityFacts>,
    ) -> Result<CompletedCatalog> {
        let result = self.finish_inner(projection, facts);
        if result.is_err() {
            self.failed();
            // A complete header is never authority until root selects the sibling.
            // Best-effort downgrade also covers errors after the final commit/close.
            if self.connection.is_none() {
                self.connection =
                    Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_WRITE).ok();
            }
            if let Some(connection) = &self.connection {
                let _ = connection.progress_handler(0, None::<fn() -> bool>);
                let _ = connection.execute(
                    "UPDATE catalog_meta SET state='building' WHERE singleton=1",
                    [],
                );
                let _ = checkpoint(connection);
            }
        }
        result
    }
    fn finish_inner(
        &mut self,
        projection: &ValidationProjection,
        facts: Option<&super::eligibility_facts::NormalizedEligibilityFacts>,
    ) -> Result<CompletedCatalog> {
        self.guard()?;
        if projection.vault_id != self.identity.selection.vault_id {
            return Err(WikiError::new(
                ErrorCode::IndexCorrupt,
                "validation projection belongs to another vault",
            ));
        }
        if let Some(facts) = facts {
            self.insert_eligibility_facts(projection, facts)?;
        } else if self.stats.registry_keys != 0 || self.stats.link_facts != 0 {
            return Err(WikiError::new(
                ErrorCode::IndexCorrupt,
                "normalized emitted facts require normalized finalization",
            ));
        }
        for (id, row) in &projection.records {
            if id != row.record.id() {
                return Err(WikiError::new(
                    ErrorCode::IndexCorrupt,
                    "record key disagrees with canonical identity",
                ));
            }
            let size = counted_json(row, self.limits.max_row_bytes)?;
            self.admit(checked_sum(&[
                512,
                size,
                id.as_str().len() as u64,
                row.record.kind().as_str().len() as u64,
                row.path.as_str().len() as u64,
                row.hash.as_str().len() as u64,
                row.authored_status.as_ref().map_or(0, String::len) as u64,
                eligibility(row.eligibility).len() as u64,
                row.identity_eligibility.map_or(0, |v| eligibility(v).len()) as u64,
                row.description_eligibility
                    .map_or(0, |v| eligibility(v).len()) as u64,
            ])?)?;
            let row_json = sql::json(row)?;
            self.connection()
                .execute(
                    "INSERT INTO records VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                    params![
                        id.as_str(),
                        row.record.kind().as_str(),
                        row.path.as_str(),
                        row.hash.as_str(),
                        row.authored_status,
                        eligibility(row.eligibility),
                        row.identity_eligibility.map(eligibility),
                        row.description_eligibility.map(eligibility),
                        row.disputed,
                        row_json
                    ],
                )
                .map_err(build_sql_error)?;
            self.stats.records += 1;
        }
        self.insert_refresh_lookups(projection)?;
        let revision_ownership_hash = self.reconstruct_revision_owners()?;
        for dependency in &projection.dependencies {
            let hash = match &dependency.expected {
                ExpectedState::Absent => None,
                ExpectedState::Hash(hash) => Some(hash.as_str()),
            };
            self.admit(checked_sum(&[
                256,
                dependency.path.as_str().len() as u64,
                hash.map_or(0, str::len) as u64,
            ])?)?;
            self.connection()
                .execute(
                    "INSERT INTO dependencies(path,expected_hash) VALUES(?1,?2)",
                    params![dependency.path.as_str(), hash],
                )
                .map_err(build_sql_error)?;
            self.stats.dependencies += 1;
        }
        for diagnostic in &projection.diagnostics {
            let size = counted_json(&diagnostic.details, self.limits.max_row_bytes)?;
            self.admit(checked_sum(&[
                512,
                size,
                diagnostic.path.as_str().len() as u64,
                diagnostic
                    .record_id
                    .as_ref()
                    .map_or(0, |v| v.as_str().len()) as u64,
                diagnostic.code.to_string().len() as u64,
            ])?)?;
            self.connection()
                .execute(
                    "INSERT INTO diagnostics(path,record_id,code,details_json) VALUES(?1,?2,?3,?4)",
                    params![
                        diagnostic.path.as_str(),
                        diagnostic.record_id.as_ref().map(|v| v.as_str()),
                        diagnostic.code.to_string(),
                        sql::json(&diagnostic.details)?
                    ],
                )
                .map_err(build_sql_error)?;
            self.stats.diagnostics += 1;
        }
        self.flush()?;
        let dependency_hash = dependency_fingerprint(&projection.dependencies)?;
        self.guard()?;
        self.connection()
            .execute(
                "INSERT INTO documents_fts(documents_fts,rank) VALUES('integrity-check',1)",
                [],
            )
            .map_err(build_sql_error)?;
        self.connection()
            .execute(
                "INSERT INTO graph_fts(graph_fts,rank) VALUES('integrity-check',1)",
                [],
            )
            .map_err(build_sql_error)?;
        let quick: String = self
            .connection()
            .query_row("PRAGMA quick_check", [], |r| r.get(0))
            .map_err(build_sql_error)?;
        if quick != "ok" {
            return Err(WikiError::new(ErrorCode::IndexCorrupt, quick));
        }
        let violation: Option<String> = self
            .connection()
            .query_row("PRAGMA foreign_key_check", [], |r| r.get(0))
            .optional()
            .map_err(build_sql_error)?;
        if violation.is_some() {
            return Err(WikiError::new(
                ErrorCode::IndexCorrupt,
                "normalized catalog foreign key violation",
            ));
        }
        self.step(BuildCheckpoint::BeforeComplete)?;
        // This names a publication, not a fresh full-vault proof. Subsequent
        // deltas chain their exact changed inputs without recomputing a global
        // digest. The full-build observations are separately epoch-bound.
        let mut publication_hash = Blake3Hash::digest(sql::json(&(
            "lwiki.catalog-publication.v3.build",
            &self.identity.selection,
            self.identity
                .origin
                .as_ref()
                .map(|origin| (&origin.change_id, &origin.manifest_hash)),
            &projection.parser_fingerprint,
            &projection.control_manifest,
            &dependency_hash,
            &revision_ownership_hash,
        ))?);
        if facts.is_some() {
            if self.stats.links != self.stats.link_facts {
                return Err(WikiError::new(
                    ErrorCode::IndexCorrupt,
                    "normalized link facts do not cover emitted links",
                ));
            }
            let missing: bool = self.connection().query_row(
                "SELECT EXISTS(SELECT 1 FROM records r WHERE NOT EXISTS(SELECT 1 FROM registry_match_keys k WHERE k.kind='id' AND k.value=r.id AND k.record_id=r.id AND k.path=r.path))",
                [], |row| row.get(0),
            ).map_err(build_sql_error)?;
            if missing {
                return Err(WikiError::new(
                    ErrorCode::IndexCorrupt,
                    "normalized registry facts omit adopted identity",
                ));
            }
            // This additional commitment names the proof layout and exact facts;
            // legacy full-proof builds retain their existing publication encoding.
            publication_hash = Blake3Hash::digest(sql::json(&(
                "lwiki.normalized-proof-layout.v2",
                &publication_hash,
                self.lookup_hash.finalize().to_hex().to_string(),
            ))?);
            self.connection().execute("UPDATE catalog_meta SET proof_layout_version=2 WHERE singleton=1 AND state='building'", []).map_err(build_sql_error)?;
        }
        self.connection().execute("UPDATE catalog_meta SET parser_hash=?1,control_hash=?2,dependency_hash=?3,publication_hash=?4,audit_epoch=epoch,state='complete' WHERE singleton=1 AND state='building'",params![projection.parser_fingerprint.as_str(),projection.control_manifest.as_str(),dependency_hash.as_str(),publication_hash.as_str()]).map_err(build_sql_error)?;
        self.step(BuildCheckpoint::AfterComplete)?;
        checkpoint(self.connection())?;
        self.step(BuildCheckpoint::BeforeSeal)?;
        // Prepare normal serving before selection, so the first document delta
        // does not have to wait for every reader to release a rollback database.
        super::selector::configure_wal(self.connection())?;
        let connection = self.connection.take().expect("open builder connection");
        connection
            .close()
            .map_err(|(_, error)| sql::sql_error(error))?;
        self.step(BuildCheckpoint::BeforeSync)?;
        let relative = VaultRelativePath::new(format!(
            ".wiki/cache/catalogs/{}.sqlite",
            self.identity.selection.file_id
        ))?;
        if self.fs.root().resolve(&relative)? != self.path {
            return Err(WikiError::invalid("catalog sibling binding changed"));
        }
        let file = File::open(&self.path).map_err(|e| io_error("open sealed catalog", e))?;
        self.stats.database_bytes = file
            .metadata()
            .map_err(|e| io_error("inspect sealed catalog", e))?
            .len();
        if self.stats.database_bytes > self.limits.max_database_bytes {
            return Err(budget("catalog database exceeds page limit"));
        }
        let io = self.fs.durable_io();
        io.sync_file(&file)
            .map_err(|e| io_error("sync sealed catalog", e))?;
        require_sync(
            io.sync_directory(self.path.parent().expect("catalog parent"))
                .map_err(|e| io_error("sync sealed catalog parent", e))?,
        )?;
        self.step(BuildCheckpoint::AfterSync)?;
        self.stats.elapsed_ms = self.started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        Ok(CompletedCatalog {
            identity: self.identity.clone(),
            snapshot: ReadSnapshot::published(
                self.identity.selection.creation_epoch,
                projection.parser_fingerprint.clone(),
                self.identity.selection.file_id.clone(),
                publication_hash,
            )?,
            dependency_hash,
            stats: self.stats.clone(),
            path: self.path.clone(),
        })
    }
}
impl RetrievalSink for NormalizedBuilder<'_> {
    fn link_fact(&mut self, row: super::link_facts::OwnedLinkFact) -> Result<()> {
        let result = self.insert_link_fact(&row);
        if result.is_err() {
            self.failed();
        }
        result
    }
    fn registry_keys(
        &mut self,
        entry: &crate::records::RegistryEntry,
        keys: &[super::link_facts::MatchKey],
    ) -> Result<()> {
        let result = self.insert_registry_keys(entry, keys);
        if result.is_err() {
            self.failed();
        }
        result
    }
    fn identity_claim(&mut self, row: IdentityClaimRow) -> Result<()> {
        let result = (|| {
            self.guard()?;
            self.admit(checked_sum(&[
                256,
                row.id.as_str().len() as u64,
                row.path.as_str().len() as u64,
                row.hash.as_str().len() as u64,
            ])?)?;
            self.connection().execute("INSERT INTO identity_claims(record_id,path,file_hash,kind) VALUES(?1,?2,?3,?4)", params![row.id.as_str(),row.path.as_str(),row.hash.as_str(),row.kind.map(|kind|kind.as_str())]).map_err(build_sql_error)?;
            self.stats.identity_claims += 1;
            Ok(())
        })();
        if result.is_err() {
            self.failed();
        }
        result
    }
    fn document(&mut self, row: DocumentRow) -> Result<()> {
        let result = self.insert_document(&row);
        if result.is_err() {
            self.failed();
        }
        result
    }
    fn graph(&mut self, row: GraphRow) -> Result<()> {
        let result = self.insert_graph(&row);
        if result.is_err() {
            self.failed();
        }
        result
    }
    fn link(&mut self, row: LinkRow) -> Result<()> {
        let result = self.insert_link(&row);
        if result.is_err() {
            self.failed();
        }
        result
    }
}
impl Drop for NormalizedBuilder<'_> {
    fn drop(&mut self) {
        if self.transaction_open
            && let Some(connection) = &self.connection
        {
            let _ = connection.execute_batch("ROLLBACK");
        }
    }
}

fn checkpoint(connection: &Connection) -> Result<()> {
    let (busy, log, done): (i64, i64, i64) = connection
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .map_err(build_sql_error)?;
    if busy != 0 || (log >= 0 && log != done) {
        return Err(WikiError::new(
            ErrorCode::IndexCorrupt,
            "catalog WAL checkpoint did not complete",
        ));
    }
    Ok(())
}
fn eligibility(value: Eligibility) -> &'static str {
    match value {
        Eligibility::Current => "current",
        Eligibility::Historical => "historical",
        Eligibility::Stale => "stale",
        Eligibility::Invalid => "invalid",
        Eligibility::Withdrawn => "withdrawn",
        Eligibility::Unsupported => "unsupported",
    }
}
fn budget(message: &str) -> WikiError {
    WikiError::new(ErrorCode::BudgetExceeded, message)
}
fn build_sql_error(error: rusqlite::Error) -> WikiError {
    if error.sqlite_error_code() == Some(rusqlite::ErrorCode::DiskFull) {
        return budget("normalized catalog exhausted database page or storage allowance");
    }
    sql::sql_error(error)
}
fn require_sync(sync: crate::vault::DirectorySync) -> Result<()> {
    if sync == crate::vault::DirectorySync::Unsupported {
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "normalized catalog requires supported directory durability",
        ));
    }
    Ok(())
}
fn io_error(action: &str, error: std::io::Error) -> WikiError {
    WikiError::new(ErrorCode::Internal, format!("{action}: {error}"))
}
fn checked_sum(values: &[u64]) -> Result<u64> {
    values.iter().try_fold(0u64, |sum, value| {
        sum.checked_add(*value)
            .ok_or_else(|| budget("catalog row size overflow"))
    })
}
fn joined_size(values: &[String]) -> Result<u64> {
    values
        .iter()
        .try_fold(values.len().saturating_sub(1) as u64, |sum, value| {
            sum.checked_add(value.len() as u64)
                .ok_or_else(|| budget("catalog joined text size overflow"))
        })
}
struct CountWriter {
    bytes: u64,
    limit: u64,
    exceeded: bool,
}
impl Write for CountWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        match self.bytes.checked_add(bytes.len() as u64) {
            Some(total) if total <= self.limit => {
                self.bytes = total;
                Ok(bytes.len())
            }
            _ => {
                self.exceeded = true;
                Err(std::io::Error::other("catalog serialized row limit"))
            }
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn counted_json<T: Serialize + ?Sized>(value: &T, limit: u64) -> Result<u64> {
    let mut writer = CountWriter {
        bytes: 0,
        limit,
        exceeded: false,
    };
    if let Err(error) = serde_json::to_writer(&mut writer, value) {
        return Err(if writer.exceeded {
            budget("catalog serialized row exceeds limit")
        } else {
            WikiError::new(ErrorCode::Internal, error.to_string())
        });
    }
    Ok(writer.bytes)
}
struct HashWriter(blake3::Hasher);
impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(crate) fn dependency_fingerprint(
    dependencies: &[crate::changes::ReadDependency],
) -> Result<Blake3Hash> {
    let mut writer = HashWriter(blake3::Hasher::new());
    serde_json::to_writer(&mut writer, dependencies)
        .map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))?;
    Blake3Hash::new(format!("blake3:{}", writer.0.finalize().to_hex()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        catalog::file_types::CatalogSelection,
        changes::{ReadDependency, ScanDocument, ValidationInput},
        domain::{RecordId, RecordKind},
        vault::VaultRoot,
    };

    fn fixture() -> (tempfile::TempDir, VaultFs, WriterPermit, BuildIdentity) {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("WIKI.md"),
            "---\nwiki_schema: \"1\"\nwiki_id: vault_fixture\nwiki_kind: vault\ntitle: Disposable builder fixture\n---\nFixture\n",
        )
        .unwrap();
        let fs = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let writer = WriterPermit::acquire(fs.root(), Duration::from_millis(100)).unwrap();
        let identity = BuildIdentity {
            selection: CatalogSelection::new(RecordId::new("vault_fixture").unwrap(), 1).unwrap(),
            origin: None,
            vector_cache_lost: false,
            vector_loss_unknown: false,
        };
        (temp, fs, writer, identity)
    }
    fn document() -> DocumentRow {
        DocumentRow {
            path: VaultRelativePath::new("plain.md").unwrap(),
            hash: Blake3Hash::digest("# Café\nneedle body\n"),
            record_id: Some(RecordId::new("document_fixture").unwrap()),
            kind: Some(RecordKind::Page),
            title: "Café title".into(),
            aliases: vec!["alternate spelling".into(), "quoted \"alias\"".into()],
            headings: "Heading needle".into(),
            tags: vec!["tag-one".into(), "two".into()],
            body: "needle body".into(),
            raw_text: "# Café\nneedle body\n".into(),
            source_id: Some(RecordId::new("source_fixture").unwrap()),
            owner_revision: Some(RecordId::new("revision_fixture").unwrap()),
            eligibility: Eligibility::Current,
            reasons: vec!["because".into()],
        }
    }
    fn validation(identity: &BuildIdentity) -> ValidationProjection {
        ValidationProjection {
            vault_id: identity.selection.vault_id.clone(),
            parser_fingerprint: Blake3Hash::digest("parser"),
            control_manifest: Blake3Hash::digest("control"),
            records: Default::default(),
            diagnostics: vec![],
            dependencies: vec![
                ReadDependency {
                    path: VaultRelativePath::new("z.md").unwrap(),
                    expected: ExpectedState::Absent,
                },
                ReadDependency {
                    path: VaultRelativePath::new("a.md").unwrap(),
                    expected: ExpectedState::Hash(Blake3Hash::digest("a")),
                },
            ],
        }
    }
    fn read(path: &std::path::Path) -> Connection {
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
    }
    fn state(path: &std::path::Path) -> String {
        read(path)
            .query_row("SELECT state FROM catalog_meta", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn wal_ready_rows_and_external_fts_preserve_all_document_fields() {
        let (_temp, fs, writer, identity) = fixture();
        let mut builder = NormalizedBuilder::begin(
            &fs,
            &writer,
            identity.clone(),
            BuildLimits {
                max_batch_rows: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let d = document();
        builder.document(d.clone()).unwrap();
        let g = GraphRow {
            target_id: RecordId::new("entity_a").unwrap(),
            target_kind: RecordKind::Entity,
            name: "Graph café".into(),
            aliases: vec!["alternate graph".into()],
            endpoints: "entity_a entity_b".into(),
            predicate: "connects".into(),
            qualifiers: "year 2026".into(),
            description: "uniquegraphword".into(),
        };
        builder.graph(g.clone()).unwrap();
        let link = LinkRow {
            from_path: d.path.clone(),
            byte_start: 5,
            target_id: Some(g.target_id.clone()),
            target_path: Some(VaultRelativePath::new("graph.md").unwrap()),
            resolution: "resolved".into(),
        };
        builder.link(link.clone()).unwrap();
        let validation = validation(&identity);
        let complete = builder.finish(&validation).unwrap();
        assert_eq!(
            complete.dependency_hash,
            Blake3Hash::digest(serde_json::to_vec(&validation.dependencies).unwrap())
        );
        assert!(complete.stats.batches >= 5);
        assert_eq!(
            complete.stats.largest_batch_bytes,
            complete.stats.largest_row_bytes
        );
        let c = read(&complete.path);
        assert_eq!(state(&complete.path), "complete");
        let got=c.query_row("SELECT path,file_hash,record_id,kind,title,aliases_json,aliases_text,headings,tags_json,tags_text,body,raw_text,source_id,owner_revision,eligibility,reasons_json FROM documents",[],|r|{
            Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?,r.get::<_,String>(7)?,r.get::<_,String>(8)?,r.get::<_,String>(9)?,r.get::<_,String>(10)?,r.get::<_,String>(11)?,r.get::<_,Option<String>>(12)?,r.get::<_,Option<String>>(13)?,r.get::<_,String>(14)?,r.get::<_,String>(15)?))
        }).unwrap();
        assert_eq!(got.0, d.path.as_str());
        assert_eq!(got.1, d.hash.as_str());
        assert_eq!(got.2.as_deref(), d.record_id.as_ref().map(|v| v.as_str()));
        assert_eq!(got.3.as_deref(), d.kind.map(|v| v.as_str()));
        assert_eq!(got.4, d.title);
        assert_eq!(got.5, sql::json(&d.aliases).unwrap());
        assert_eq!(got.6, d.aliases.join(" "));
        assert_eq!(got.7, d.headings);
        assert_eq!(got.8, sql::json(&d.tags).unwrap());
        assert_eq!(got.9, d.tags.join(" "));
        assert_eq!(got.10, d.body);
        assert_eq!(got.11, d.raw_text);
        assert_eq!(got.12.as_deref(), d.source_id.as_ref().map(|v| v.as_str()));
        assert_eq!(
            got.13.as_deref(),
            d.owner_revision.as_ref().map(|v| v.as_str())
        );
        assert_eq!(got.14, "current");
        assert_eq!(got.15, sql::json(&d.reasons).unwrap());
        for term in ["cafe", "alternate", "needle", "tag"] {
            assert_eq!(
                c.query_row(
                    "SELECT count(*) FROM documents_fts WHERE documents_fts MATCH ?1",
                    [term],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                1
            );
        }
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM graph_fts WHERE graph_fts MATCH 'uniquegraphword'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        let graph_aliases: (String, String) = c
            .query_row(
                "SELECT aliases_json,aliases_text FROM graph_rows",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            graph_aliases,
            (sql::json(&g.aliases).unwrap(), g.aliases.join(" "))
        );
        let dependency_paths: Vec<String> = c
            .prepare("SELECT path FROM dependencies ORDER BY dependency_row")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(dependency_paths, vec!["z.md", "a.md"]);
        let document_columns: Vec<String> = c
            .prepare("PRAGMA table_info(documents)")
            .unwrap()
            .query_map([], |r| r.get(1))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert!(!document_columns.iter().any(|v| v == "row_json"));
        let tables: Vec<String> = c
            .prepare("SELECT name FROM sqlite_schema WHERE type='table'")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert!(!tables.iter().any(|v| matches!(
            v.as_str(),
            "generations" | "entities" | "assertions" | "sources"
        )));
        assert_eq!(
            c.query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "wal"
        );
        for suffix in ["-wal", "-shm"] {
            assert!(PathBuf::from(format!("{}{suffix}", complete.path.display())).exists());
        }
        assert!(!PathBuf::from(format!("{}-journal", complete.path.display())).exists());
        drop(c);
        assert!(PathBuf::from(format!("{}-wal", complete.path.display())).exists());
    }

    #[test]
    fn row_limit_errors_poison_builder_and_leave_building() {
        let (_temp, fs, writer, identity) = fixture();
        let mut builder = NormalizedBuilder::begin(
            &fs,
            &writer,
            identity.clone(),
            BuildLimits {
                max_row_bytes: 1024,
                max_batch_bytes: 1024,
                ..Default::default()
            },
        )
        .unwrap();
        let path = builder.path.clone();
        let mut large = document();
        large.aliases = vec!["\"".repeat(2000)];
        assert_eq!(
            builder.document(large).unwrap_err().code,
            ErrorCode::BudgetExceeded
        );
        assert_eq!(
            builder.document(document()).unwrap_err().code,
            ErrorCode::IndexCorrupt
        );
        assert!(builder.finish(&validation(&identity)).is_err());
        assert_eq!(state(&path), "building");
        assert_eq!(
            read(&path)
                .query_row("SELECT count(*) FROM documents", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn bounded_batches_commit_and_checkpoint_without_retaining_rows() {
        let (_temp, fs, writer, identity) = fixture();
        let mut builder = NormalizedBuilder::begin(
            &fs,
            &writer,
            identity.clone(),
            BuildLimits {
                max_row_bytes: 1024,
                max_batch_bytes: 1024,
                max_batch_rows: 256,
                ..Default::default()
            },
        )
        .unwrap();
        builder.document(document()).unwrap();
        let mut second = document();
        second.path = VaultRelativePath::new("second.md").unwrap();
        second.record_id = Some(RecordId::new("second_fixture").unwrap());
        builder.document(second).unwrap();
        assert_eq!(builder.stats.batches, 1);
        assert_eq!(builder.batch_rows, 1);
        assert!(builder.stats.largest_batch_bytes <= 1024);
        let complete = builder.finish(&validation(&identity)).unwrap();
        assert_eq!(complete.stats.documents, 2);
        assert!(complete.stats.largest_batch_bytes <= 1024);
    }

    struct Fail(BuildCheckpoint);
    impl BuildFault for Fail {
        fn check(&self, step: BuildCheckpoint) -> Result<()> {
            if step == self.0 {
                Err(WikiError::new(
                    ErrorCode::Cancelled,
                    "injected builder fault",
                ))
            } else {
                Ok(())
            }
        }
    }
    #[test]
    fn ordinary_and_fts_are_atomic_and_all_failure_steps_leave_building() {
        for step in [
            BuildCheckpoint::AfterOrdinaryRow,
            BuildCheckpoint::AfterFtsRow,
            BuildCheckpoint::BeforeBatchCommit,
            BuildCheckpoint::AfterBatchCheckpoint,
            BuildCheckpoint::BeforeComplete,
            BuildCheckpoint::AfterComplete,
            BuildCheckpoint::BeforeSeal,
            BuildCheckpoint::BeforeSync,
            BuildCheckpoint::AfterSync,
        ] {
            let (_temp, fs, writer, identity) = fixture();
            let mut builder = NormalizedBuilder::begin(
                &fs,
                &writer,
                identity.clone(),
                BuildLimits {
                    fault: Some(Arc::new(Fail(step))),
                    ..Default::default()
                },
            )
            .unwrap();
            let path = builder.path.clone();
            let callback = builder.document(document());
            if matches!(
                step,
                BuildCheckpoint::AfterOrdinaryRow | BuildCheckpoint::AfterFtsRow
            ) {
                assert!(callback.is_err());
            } else {
                callback.unwrap();
            }
            assert!(builder.finish(&validation(&identity)).is_err(), "{step:?}");
            assert_eq!(state(&path), "building", "{step:?}");
            let c = read(&path);
            let documents: i64 = c
                .query_row("SELECT count(*) FROM documents", [], |r| r.get(0))
                .unwrap();
            let fts: i64 = c
                .query_row(
                    "SELECT count(*) FROM documents_fts WHERE documents_fts MATCH 'needle'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(documents, fts, "{step:?}");
        }
    }

    #[test]
    fn duplicate_dependency_and_wrong_vault_cannot_complete() {
        for wrong_vault in [false, true] {
            let (_temp, fs, writer, identity) = fixture();
            let builder =
                NormalizedBuilder::begin(&fs, &writer, identity.clone(), BuildLimits::default())
                    .unwrap();
            let path = builder.path.clone();
            let mut projection = validation(&identity);
            if wrong_vault {
                projection.vault_id = RecordId::new("other_vault").unwrap();
            } else {
                projection
                    .dependencies
                    .push(projection.dependencies[0].clone());
            }
            assert!(builder.finish(&projection).is_err());
            assert_eq!(state(&path), "building");
        }
    }

    #[test]
    fn exclusive_creation_and_writer_authority() {
        let (_temp, fs, writer, identity) = fixture();
        let builder =
            NormalizedBuilder::begin(&fs, &writer, identity.clone(), BuildLimits::default())
                .unwrap();
        let path = builder.path.clone();
        drop(builder);
        let before = std::fs::read(&path).unwrap();
        assert!(
            NormalizedBuilder::begin(&fs, &writer, identity.clone(), BuildLimits::default())
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let (_other, other_fs, _other_writer, _other_identity) = fixture();
        assert!(
            NormalizedBuilder::begin(&other_fs, &writer, identity, BuildLimits::default()).is_err()
        );
    }

    #[test]
    fn stale_or_symlink_sidecar_cannot_be_adopted() {
        for suffix in ["-wal", "-shm", "-journal"] {
            let (_temp, fs, writer, identity) = fixture();
            let directory = fs.root().path().join(".wiki/cache/catalogs");
            std::fs::create_dir_all(&directory).unwrap();
            let file = directory.join(format!("{}.sqlite", identity.selection.file_id));
            let sidecar = directory.join(format!("{}.sqlite{suffix}", identity.selection.file_id));
            std::fs::write(&sidecar, "stale-sidecar").unwrap();
            assert!(
                NormalizedBuilder::begin(&fs, &writer, identity, BuildLimits::default()).is_err()
            );
            assert!(!file.exists());
            assert_eq!(std::fs::read(sidecar).unwrap(), b"stale-sidecar");
        }
        #[cfg(unix)]
        {
            let (_temp, fs, writer, identity) = fixture();
            let directory = fs.root().path().join(".wiki/cache/catalogs");
            std::fs::create_dir_all(&directory).unwrap();
            let target = fs.root().path().join("protected-target");
            std::fs::write(&target, "must-not-change").unwrap();
            let sidecar = directory.join(format!("{}.sqlite-wal", identity.selection.file_id));
            std::os::unix::fs::symlink(&target, sidecar).unwrap();
            assert!(
                NormalizedBuilder::begin(&fs, &writer, identity, BuildLimits::default()).is_err()
            );
            assert_eq!(std::fs::read(target).unwrap(), b"must-not-change");
        }
    }

    #[test]
    fn streaming_projection_record_json_and_graph_match_collecting_fixture() {
        let (_temp, fs, writer, identity) = fixture();
        let bytes = include_bytes!("../../tests/fixtures/p08/a.md").to_vec();
        let input = ValidationInput {
            vault_id: identity.selection.vault_id.clone(),
            documents: vec![ScanDocument {
                path: VaultRelativePath::new("entities/a.md").unwrap(),
                hash: Blake3Hash::digest(&bytes),
                bytes,
            }],
            overlay: vec![],
        };
        let expected = super::super::scan::project(&fs, &input).unwrap();
        let mut builder =
            NormalizedBuilder::begin(&fs, &writer, identity, BuildLimits::default()).unwrap();
        let validation =
            super::super::scan::project_with_sink(&fs, &input, false, &mut builder).unwrap();
        let complete = builder.finish(&validation).unwrap();
        let c = read(&complete.path);
        let mut legacy = Connection::open_in_memory().unwrap();
        sql::initialize(&mut legacy).unwrap();
        let tx = legacy.transaction().unwrap();
        sql::insert_projection(&tx, 1, &expected).unwrap();
        sql::replace_documents_fts(&tx, 1, &expected).unwrap();
        sql::replace_graph_fts(&tx, 1, &expected).unwrap();
        tx.commit().unwrap();
        for term in ["identity", "searchable", "unsupported", "a", "absenttoken"] {
            let old_paths: Vec<String> = legacy.prepare("SELECT d.path FROM documents_fts f JOIN documents d ON d.doc_row=f.doc_row AND d.gen=f.gen WHERE documents_fts MATCH ?1 ORDER BY d.path").unwrap().query_map([term], |r|r.get(0)).unwrap().collect::<std::result::Result<_,_>>().unwrap();
            let new_paths: Vec<String> = c.prepare("SELECT d.path FROM documents_fts f JOIN documents d ON d.doc_row=f.rowid WHERE documents_fts MATCH ?1 ORDER BY d.path").unwrap().query_map([term], |r|r.get(0)).unwrap().collect::<std::result::Result<_,_>>().unwrap();
            assert_eq!(new_paths, old_paths, "document FTS {term}");
            let old_ids: Vec<String> = legacy
                .prepare(
                    "SELECT target_id FROM graph_fts WHERE graph_fts MATCH ?1 ORDER BY target_id",
                )
                .unwrap()
                .query_map([term], |r| r.get(0))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap();
            let new_ids: Vec<String> = c
                .prepare(
                    "SELECT target_id FROM graph_fts WHERE graph_fts MATCH ?1 ORDER BY target_id",
                )
                .unwrap()
                .query_map([term], |r| r.get(0))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap();
            assert_eq!(new_ids, old_ids, "graph FTS {term}");
        }
        assert_eq!(complete.stats.documents, expected.documents.len() as u64);
        assert_eq!(complete.stats.graph_rows, expected.graph.len() as u64);
        for (id, row) in expected.records {
            let encoded: String = c
                .query_row(
                    "SELECT row_json FROM records WHERE id=?1",
                    [id.as_str()],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(serde_json::from_str::<RecordRow>(&encoded).unwrap(), row);
        }
        for row in expected.graph {
            let got:(String,String,String,String,String,String,String)=c.query_row("SELECT target_kind,name,aliases_json,endpoints,predicate,qualifiers,description FROM graph_rows WHERE target_id=?1",[row.target_id.as_str()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).unwrap();
            assert_eq!(
                got,
                (
                    row.target_kind.to_string(),
                    row.name,
                    sql::json(&row.aliases).unwrap(),
                    row.endpoints,
                    row.predicate,
                    row.qualifiers,
                    row.description
                )
            );
        }
    }

    #[test]
    fn count_and_deadline_limits_cannot_be_ignored() {
        let (_temp, fs, writer, identity) = fixture();
        let mut builder = NormalizedBuilder::begin(
            &fs,
            &writer,
            identity.clone(),
            BuildLimits {
                max_rows: 1,
                ..Default::default()
            },
        )
        .unwrap();
        builder.document(document()).unwrap();
        assert_eq!(
            builder
                .graph(GraphRow {
                    target_id: RecordId::new("a").unwrap(),
                    target_kind: RecordKind::Entity,
                    name: "a".into(),
                    aliases: vec![],
                    endpoints: String::new(),
                    predicate: String::new(),
                    qualifiers: String::new(),
                    description: String::new()
                })
                .unwrap_err()
                .code,
            ErrorCode::BudgetExceeded
        );
        assert!(builder.finish(&validation(&identity)).is_err());
        let mut builder = NormalizedBuilder::begin(
            &fs,
            &writer,
            BuildIdentity {
                selection: CatalogSelection::new(identity.selection.vault_id, 2).unwrap(),
                ..identity
            },
            BuildLimits::default(),
        )
        .unwrap();
        builder.started = Instant::now() - Duration::from_secs(5 * 60 * 60);
        assert_eq!(
            builder.document(document()).unwrap_err().code,
            ErrorCode::BudgetExceeded
        );
        assert!(builder.poisoned);
    }

    #[test]
    fn primary_page_bound_reports_budget_and_cannot_complete() {
        let (_temp, fs, writer, identity) = fixture();
        let mut builder = NormalizedBuilder::begin(
            &fs,
            &writer,
            identity.clone(),
            BuildLimits {
                max_database_bytes: 512 * 1024,
                ..Default::default()
            },
        )
        .unwrap();
        let path = builder.path.clone();
        let mut row = document();
        row.raw_text = "a".repeat(1024 * 1024);
        assert_eq!(
            builder.document(row).unwrap_err().code,
            ErrorCode::BudgetExceeded
        );
        assert!(builder.finish(&validation(&identity)).is_err());
        assert_eq!(state(&path), "building");
        assert!(std::fs::metadata(path).unwrap().len() <= 512 * 1024);
    }

    struct SyncFault {
        file: std::sync::atomic::AtomicBool,
        directory: std::sync::atomic::AtomicBool,
    }
    impl crate::vault::DurableIo for SyncFault {
        fn create_stage(&self, path: &std::path::Path) -> std::io::Result<File> {
            crate::vault::NativeIo.create_stage(path)
        }
        fn open_append(&self, path: &std::path::Path) -> std::io::Result<File> {
            crate::vault::NativeIo.open_append(path)
        }
        fn truncate_file(&self, file: &File, length: u64) -> std::io::Result<()> {
            crate::vault::NativeIo.truncate_file(file, length)
        }
        fn write_stage(&self, file: &mut File, bytes: &[u8]) -> std::io::Result<()> {
            crate::vault::NativeIo.write_stage(file, bytes)
        }
        fn sync_file(&self, file: &File) -> std::io::Result<()> {
            if self.file.load(std::sync::atomic::Ordering::Relaxed) {
                Err(std::io::Error::other("injected sync failure"))
            } else {
                crate::vault::NativeIo.sync_file(file)
            }
        }
        fn replace(
            &self,
            stage: &std::path::Path,
            target: &std::path::Path,
        ) -> std::io::Result<()> {
            crate::vault::NativeIo.replace(stage, target)
        }
        fn remove(&self, path: &std::path::Path) -> std::io::Result<()> {
            crate::vault::NativeIo.remove(path)
        }
        fn create_directory(&self, path: &std::path::Path) -> std::io::Result<()> {
            crate::vault::NativeIo.create_directory(path)
        }
        fn sync_directory(
            &self,
            path: &std::path::Path,
        ) -> std::io::Result<crate::vault::DirectorySync> {
            if self.directory.load(std::sync::atomic::Ordering::Relaxed) {
                Ok(crate::vault::DirectorySync::Unsupported)
            } else {
                crate::vault::NativeIo.sync_directory(path)
            }
        }
    }
    #[test]
    fn io_sync_failures_and_unsupported_directory_durability_cannot_complete() {
        for unsupported_directory in [false, true] {
            let (_temp, fs, writer, identity) = fixture();
            let fault = Arc::new(SyncFault {
                file: false.into(),
                directory: false.into(),
            });
            let fs = VaultFs::with_io(fs.root().clone(), fault.clone());
            let mut builder =
                NormalizedBuilder::begin(&fs, &writer, identity.clone(), BuildLimits::default())
                    .unwrap();
            let path = builder.path.clone();
            builder.document(document()).unwrap();
            if unsupported_directory {
                fault
                    .directory
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            } else {
                fault.file.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            let error = builder
                .finish(&validation(&identity))
                .err()
                .expect("durability failure cannot complete");
            assert_eq!(
                error.code,
                if unsupported_directory {
                    ErrorCode::CapabilityUnavailable
                } else {
                    ErrorCode::Internal
                }
            );
            assert_eq!(state(&path), "building");
        }
    }
}

//! Bounded command-local normalized observation. This never opens a cache file,
//! reserves an epoch, acquires a writer, or publishes durable catalog metadata.
use super::{
    Catalog,
    maintenance_input::MaintenanceInput,
    maintenance_types::{MaintenanceLimits, MaintenanceUsage},
    normalized_build::{BuildCheckpoint, BuildLimits, BuildStats, NormalizedRows},
    query::QuerySnapshot,
    query_types::QueryReadLimits,
    scan, sql,
};
use crate::{
    changes::operation_authority::Authority,
    domain::{Blake3Hash, ErrorCode, ReadSnapshot, Result, WikiError},
};
use rusqlite::Connection;
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

pub(crate) enum CurrentQueryAttempt {
    Ready(Box<QuerySnapshot>),
    /// Only an actual resource refusal permits published discovery fallback.
    /// Invalid authority or changed canonical evidence always remains an error.
    BoundedUnavailable(WikiError),
}

pub(crate) struct CurrentObservation {
    input: MaintenanceInput,
    pub(crate) fingerprint: Blake3Hash,
    pub(crate) observed_at: String,
    pub(crate) known_issues: bool,
    pub(crate) build_stats: BuildStats,
    pub(crate) construction_rows: u64,
    sql_progress: Arc<AtomicU64>,
}
impl CurrentObservation {
    /// One progress callback per requested VM operation, including construction
    /// and later selection. Interrupt callbacks may count the stopping step.
    pub(crate) fn sql_progress_callbacks(&self) -> u64 {
        self.sql_progress.load(Ordering::Relaxed)
    }
    pub(crate) fn usage(&self) -> MaintenanceUsage {
        self.input.usage()
    }
    pub(crate) fn remaining_work(&self) -> Result<(usize, usize, u64)> {
        self.input.remaining_external_work()
    }
    pub(crate) fn charge_work(&self, bytes: usize, files: usize, entries: usize) -> Result<()> {
        self.input.charge_external_work(bytes, files, entries)
    }
    pub(super) fn final_recheck(&self) -> Result<()> {
        self.input.final_recheck()
    }
}

pub(super) fn verify_authority(catalog: &Catalog, captured: &Authority) -> Result<()> {
    let current = catalog.operation_state()?.ok_or_else(|| {
        WikiError::new(
            ErrorCode::RecoveryRequired,
            "current observation operation authority disappeared",
        )
    })?;
    current.require_publication(current.publication())?;
    if !captured.same_revision(&current) {
        return Err(WikiError::new(
            ErrorCode::ContentConflict,
            "operation authority changed during current note discovery; retry the lookup",
        ));
    }
    Ok(())
}

fn input_limits(max_elapsed: Duration) -> MaintenanceLimits {
    MaintenanceLimits {
        max_files: 4096,
        max_path_steps: 65_536,
        max_manifest_bytes: 4 * 1024 * 1024,
        max_file_bytes: 8 * 1024 * 1024,
        max_retained_note_bytes: 16 * 1024 * 1024,
        max_io_bytes: 64 * 1024 * 1024,
        max_elapsed,
    }
}

impl Catalog {
    pub(crate) fn current_query_snapshot(
        &self,
        mut limits: QueryReadLimits,
    ) -> Result<CurrentQueryAttempt> {
        limits.validate()?;
        limits.max_elapsed_ms = limits.max_elapsed_ms.min(5000);
        let started = Instant::now();
        // Operation state is retained authority, independently of cache presence.
        // In particular, no budget fallback can erase a missing or active slot.
        let authority = self.operation_state()?.ok_or_else(|| {
            WikiError::new(
                ErrorCode::RecoveryRequired,
                "normalized current discovery requires operation authority",
            )
        })?;
        authority.require_publication(authority.publication())?;
        let result = self.build_current_query(limits, started, authority.clone());
        match result {
            Ok(reader) => Ok(CurrentQueryAttempt::Ready(Box::new(reader))),
            Err(error) if error.code == ErrorCode::BudgetExceeded => {
                // An operation beginning (or completing) during an incomplete
                // observation is a real conflict, not cache availability.
                verify_authority(self, &authority)?;
                let mut unavailable = WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "current note discovery exceeded its bounded read allowance",
                );
                unavailable.hint = Some("use the published results, or run index sync from a writable session to refresh discovery".into());
                unavailable.details =
                    serde_json::json!({"scope":"current_observation", "cause":error});
                Ok(CurrentQueryAttempt::BoundedUnavailable(unavailable))
            }
            Err(error) => Err(error),
        }
    }

    fn build_current_query(
        &self,
        limits: QueryReadLimits,
        started: Instant,
        authority: Authority,
    ) -> Result<QuerySnapshot> {
        let remaining = Duration::from_millis(limits.max_elapsed_ms)
            .checked_sub(started.elapsed())
            .filter(|time| !time.is_zero())
            .ok_or_else(|| budget("current observation deadline exhausted"))?;
        let input = MaintenanceInput::capture(&self.fs, &self.vault_id, input_limits(remaining))?;
        // The projection's vault ID is supplied by its caller. Bind that ID to
        // the actual captured marker before constructing any derived rows.
        let marker = input
            .notes()
            .get(&crate::domain::VaultRelativePath::new("WIKI.md")?)
            .and_then(|note| note.canonical.as_ref());
        if !marker.is_some_and(|record| {
            record.kind() == crate::domain::RecordKind::Vault && record.id() == &self.vault_id
        }) {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "vault identity changed before current note discovery; reopen the wiki and retry",
            ));
        }
        let connection = Connection::open_in_memory().map_err(memory_sql_error)?;
        // One handler counts schema construction, FTS insertion and subsequent
        // selected queries. Never reset this allowance when returning the reader.
        let sql_progress = Arc::new(AtomicU64::new(0));
        super::query::configure_query_at(
            &connection,
            &limits,
            started,
            true,
            Some(sql_progress.clone()),
        )?;
        let mut rows = MemoryRows::new(connection, started, &limits)?;
        let projection = scan::project_maintenance_with_sink(&input, &mut rows)?;
        input.require_clean()?;
        if projection.validation.vault_id != self.vault_id {
            return Err(WikiError::invalid(
                "current observation belongs to another vault",
            ));
        }
        rows.insert_eligibility_facts(&projection.validation, &projection.facts)?;
        rows.insert_records(&projection.validation)?;
        rows.insert_refresh_lookups(&projection.validation)?;
        rows.insert_dependencies_and_diagnostics(&projection.validation)?;
        rows.flush()?;
        rows.guard()?;
        input.require_clean()?;
        verify_authority(self, &authority)?;
        let fingerprint = Blake3Hash::digest(sql::json(&(
            "lwiki.current-observation.v1",
            &self.vault_id,
            &projection.validation.parser_fingerprint,
            &projection.validation.control_manifest,
            input.observation_fingerprint()?,
            authority.revision(),
            authority.publication(),
        ))?);
        // Generation zero denotes absence of a publication; the separate
        // provenance and fingerprint bind this current canonical observation.
        let snapshot = ReadSnapshot::canonical(
            0,
            projection.validation.parser_fingerprint,
            projection.validation.control_manifest,
        );
        let pages: i64 = rows
            .connection
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .map_err(memory_sql_error)?;
        rows.stats.database_bytes = u64::try_from(pages)
            .map_err(|_| WikiError::new(ErrorCode::IndexCorrupt, "invalid in-memory page count"))?
            .checked_mul(4096)
            .ok_or_else(|| {
                WikiError::new(ErrorCode::BudgetExceeded, "in-memory page size overflow")
            })?;
        rows.stats.elapsed_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        rows.connection
            .execute_batch("PRAGMA query_only=ON;")
            .map_err(memory_sql_error)?;
        Ok(QuerySnapshot::from_current_observation(
            rows.connection,
            CurrentObservation {
                input,
                fingerprint,
                observed_at: time::OffsetDateTime::now_utc()
                    .format(&time::format_description::well_known::Rfc3339)
                    .map_err(|_| WikiError::invalid("observation UTC encoding"))?,
                known_issues: !projection.validation.diagnostics.is_empty(),
                build_stats: rows.stats,
                construction_rows: rows.total_rows,
                sql_progress,
            },
            snapshot,
            self.vault_id.clone(),
            limits,
            started,
            authority,
        ))
    }
}

/// A separate in-memory lifecycle sharing the durable builder's typed insertion
/// algorithms. There is intentionally no writer, path, selection or catalog_meta
/// row. MEMORY temp storage and journal prohibit SQLite filesystem spill files.
struct MemoryRows {
    connection: Connection,
    limits: BuildLimits,
    started: Instant,
    total_rows: u64,
    batch_rows: u64,
    batch_bytes: u64,
    transaction_open: bool,
    poisoned: bool,
    stats: BuildStats,
    lookup_hash: blake3::Hasher,
}
impl MemoryRows {
    fn new(connection: Connection, started: Instant, query: &QueryReadLimits) -> Result<Self> {
        let limits = BuildLimits {
            max_row_bytes: 32 * 1024 * 1024,
            max_batch_bytes: 32 * 1024 * 1024,
            max_batch_rows: 256,
            max_rows: 65_536,
            max_history_steps: 1,
            sqlite_cache_bytes: 8 * 1024 * 1024,
            max_database_bytes: 128 * 1024 * 1024,
            max_elapsed: Duration::from_millis(query.max_elapsed_ms),
            fault: None,
        };
        connection.execute_batch(
            "PRAGMA page_size=4096; PRAGMA max_page_count=32768; PRAGMA temp_store=MEMORY; PRAGMA journal_mode=MEMORY; PRAGMA foreign_keys=ON;",
        ).map_err(memory_sql_error)?;
        connection
            .execute_batch(super::normalized_schema::SCHEMA)
            .map_err(memory_sql_error)?;
        connection
            .execute_batch(super::unit_inventory::SCHEMA)
            .map_err(memory_sql_error)?;
        connection
            .execute_batch(super::source_relations::SCHEMA)
            .map_err(memory_sql_error)?;
        Ok(Self {
            connection,
            limits,
            started,
            total_rows: 0,
            batch_rows: 0,
            batch_bytes: 0,
            transaction_open: false,
            poisoned: false,
            stats: BuildStats::default(),
            lookup_hash: blake3::Hasher::new(),
        })
    }
}
fn memory_sql_error(error: rusqlite::Error) -> WikiError {
    if error.sqlite_error_code() == Some(rusqlite::ErrorCode::DiskFull) {
        budget("current observation in-memory SQLite page allowance exhausted")
    } else {
        sql::sql_error(error)
    }
}
fn budget(message: &str) -> WikiError {
    WikiError::new(ErrorCode::BudgetExceeded, message)
}
impl NormalizedRows for MemoryRows {
    fn connection(&self) -> &Connection {
        &self.connection
    }
    fn limits(&self) -> &BuildLimits {
        &self.limits
    }
    fn guard(&self) -> Result<()> {
        if self.poisoned {
            return Err(WikiError::new(
                ErrorCode::IndexCorrupt,
                "current observation row construction failed",
            ));
        }
        if self.started.elapsed() >= self.limits.max_elapsed {
            return Err(budget("current observation deadline exhausted"));
        }
        Ok(())
    }
    fn admit(&mut self, bytes: u64) -> Result<()> {
        self.guard()?;
        if bytes > self.limits.max_row_bytes || self.total_rows >= self.limits.max_rows {
            return Err(budget("current observation row allowance exhausted"));
        }
        let total = self
            .stats
            .admitted_bytes
            .checked_add(bytes)
            .filter(|total| *total <= 128 * 1024 * 1024)
            .ok_or_else(|| budget("current observation cumulative row bytes exhausted"))?;
        if self.batch_rows >= self.limits.max_batch_rows
            || self.batch_bytes > self.limits.max_batch_bytes - bytes
        {
            self.flush()?;
        }
        if !self.transaction_open {
            self.connection
                .execute_batch("BEGIN")
                .map_err(memory_sql_error)?;
            self.transaction_open = true;
        }
        self.total_rows += 1;
        self.batch_rows += 1;
        self.batch_bytes += bytes;
        self.stats.admitted_bytes = total;
        self.stats.largest_row_bytes = self.stats.largest_row_bytes.max(bytes);
        self.stats.largest_batch_bytes = self.stats.largest_batch_bytes.max(self.batch_bytes);
        Ok(())
    }
    fn flush(&mut self) -> Result<()> {
        self.guard()?;
        if self.transaction_open {
            self.connection
                .execute_batch("COMMIT")
                .map_err(memory_sql_error)?;
            self.transaction_open = false;
            self.stats.batches += 1;
        }
        self.batch_rows = 0;
        self.batch_bytes = 0;
        Ok(())
    }
    fn failed(&mut self) {
        self.poisoned = true;
    }
    fn step(&self, _: BuildCheckpoint) -> Result<()> {
        self.guard()
    }
    fn stats_mut(&mut self) -> &mut BuildStats {
        &mut self.stats
    }
    fn lookup_hash_mut(&mut self) -> &mut blake3::Hasher {
        &mut self.lookup_hash
    }
    fn total_rows(&self) -> u64 {
        self.total_rows
    }
    fn batch_rows(&self) -> u64 {
        self.batch_rows
    }
    fn total_rows_mut(&mut self) -> &mut u64 {
        &mut self.total_rows
    }
    fn batch_rows_mut(&mut self) -> &mut u64 {
        &mut self.batch_rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        catalog::query_types::QueryCatalog,
        changes::{
            PreparedChange,
            operation_authority::{self, Publication},
        },
        domain::{RecordId, VaultRelativePath},
        vault::{VaultFs, VaultRoot, WriterPermit},
    };
    use std::fs;

    fn page(id: &str, body: &str) -> String {
        format!(
            "---\nwiki_schema: '1'\nwiki_id: {id}\nwiki_kind: page\ntitle: {id}\nwiki_status: reviewed\n---\n{body}\n"
        )
    }
    fn fixture() -> (tempfile::TempDir, Catalog) {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("WIKI.md"), "---\nwiki_schema: '1'\nwiki_id: vault_transient\nwiki_kind: vault\ntitle: Observation\n---\n").unwrap();
        fs::write(
            temp.path().join("old.md"),
            page("page_old", "commonprobe old unrelated hit"),
        )
        .unwrap();
        let vault = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let catalog = Catalog::new(vault, RecordId::new("vault_transient").unwrap());
        let writer = WriterPermit::acquire(catalog.fs.root(), Duration::from_secs(1)).unwrap();
        catalog.rebuild_normalized(&writer).unwrap();
        (temp, catalog)
    }
    fn current(catalog: &Catalog) -> QuerySnapshot {
        match catalog
            .current_query_snapshot(QueryReadLimits::default())
            .unwrap()
        {
            CurrentQueryAttempt::Ready(reader) => *reader,
            CurrentQueryAttempt::BoundedUnavailable(error) => panic!("{error:?}"),
        }
    }
    fn hits(reader: &QuerySnapshot, term: &str) -> Vec<String> {
        let mut statement = reader.connection().prepare("SELECT d.path FROM documents_fts JOIN documents d ON d.doc_row=documents_fts.rowid WHERE documents_fts MATCH ?1 ORDER BY d.path").unwrap();
        statement
            .query_map([term], |row| row.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn ordinary_lexical_literal_and_context_use_current_rows_without_publication_header() {
        use crate::retrieval::{
            ContextOptions, ContextRequest, ContextScope, QueryPlan, SearchMode, indexed_documents,
            lexical,
        };
        let (_temp, catalog) = fixture();
        for mode in [SearchMode::Lexical, SearchMode::Literal] {
            let reader = current(&catalog);
            let plan = QueryPlan {
                mode,
                ..Default::default()
            };
            let found = lexical::search_catalog(&reader, "commonprobe", &plan).unwrap();
            assert_eq!(found.hits.len(), 1);
            let request = ContextRequest {
                scope: ContextScope::Current,
                documents: plan,
                ..Default::default()
            };
            let context = indexed_documents::context_with_reader(
                &catalog,
                &reader,
                "commonprobe",
                &request,
                &ContextOptions::default(),
            )
            .unwrap();
            assert!(!context.passages().is_empty());
        }
    }

    #[test]
    fn existing_cached_reader_preserves_same_inode_writer_concurrency() {
        let (_temp, catalog) = fixture();
        let reader = catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap();
        let path = VaultRelativePath::new("old.md").unwrap();
        assert_eq!(reader.document(&path).unwrap().unwrap().title, "page_old");
        let database = reader.connection().path().unwrap();
        let writer = Connection::open(database).unwrap();
        writer
            .execute(
                "UPDATE documents SET title='concurrent writer' WHERE path='old.md'",
                [],
            )
            .unwrap();
        assert_eq!(reader.document(&path).unwrap().unwrap().title, "page_old");
        drop(reader);
        let fresh = catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap();
        assert_eq!(
            fresh.document(&path).unwrap().unwrap().title,
            "concurrent writer"
        );
    }

    #[test]
    fn published_readers_leave_persisted_wal_and_shm_bytes_and_timestamps_unchanged() {
        let (temp, catalog) = fixture();
        let states = || {
            fs::read_dir(temp.path().join(".wiki/cache/catalogs"))
                .unwrap()
                .map(|entry| {
                    let path = entry.unwrap().path();
                    (
                        path.clone(),
                        (
                            fs::read(&path).unwrap(),
                            fs::metadata(path).unwrap().modified().unwrap(),
                        ),
                    )
                })
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        let before = states();
        for _ in 0..2 {
            let reader = catalog
                .published_read_only_query_snapshot(QueryReadLimits::default())
                .unwrap();
            assert_eq!(hits(&reader, "commonprobe"), ["old.md"]);
        }
        assert_eq!(states(), before);
    }

    #[test]
    fn discovers_new_membership_even_with_old_published_hit_and_no_cache() {
        let (temp, catalog) = fixture();
        fs::write(
            temp.path().join("new.md"),
            page("page_new", "commonprobe new correct instruction"),
        )
        .unwrap();
        let old = catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap();
        assert_eq!(hits(&old, "commonprobe"), ["old.md"]);
        drop(old);
        fs::remove_dir_all(temp.path().join(".wiki/cache")).unwrap();
        let reader = current(&catalog);
        assert_eq!(hits(&reader, "commonprobe"), ["new.md", "old.md"]);
        assert!(reader.publication_id().is_none());
        assert!(reader.persistent_generation().is_none());
        assert!(reader.snapshot().publication().is_none());
        assert_eq!(reader.query_scope(), "current_observation");
        assert_eq!(
            reader
                .connection()
                .query_row("SELECT count(*) FROM catalog_meta", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        reader.final_recheck(&catalog).unwrap();
        assert!(!temp.path().join(".wiki/cache").exists());
    }

    #[test]
    fn edits_and_removals_change_observation_fingerprint_and_membership() {
        let (temp, catalog) = fixture();
        let reader = current(&catalog);
        let fingerprint = reader.dependency_fingerprint().unwrap();
        fs::write(
            temp.path().join("old.md"),
            page("page_old", "newzirconium current instruction"),
        )
        .unwrap();
        assert_eq!(
            reader.final_recheck(&catalog).unwrap_err().code,
            ErrorCode::ContentConflict
        );
        let edited = current(&catalog);
        assert_ne!(edited.dependency_fingerprint().unwrap(), fingerprint);
        assert_eq!(hits(&edited, "newzirconium"), ["old.md"]);
        fs::remove_file(temp.path().join("old.md")).unwrap();
        assert_eq!(
            edited.final_recheck(&catalog).unwrap_err().code,
            ErrorCode::ContentConflict
        );
        let removed = current(&catalog);
        assert!(hits(&removed, "newzirconium").is_empty());
        removed.final_recheck(&catalog).unwrap();
    }

    #[test]
    fn changed_vault_marker_after_catalog_construction_refuses_before_projection() {
        let (temp, catalog) = fixture();
        let authority = catalog.operation_state().unwrap().unwrap();
        let path = temp.path().join("WIKI.md");
        let original = fs::read_to_string(&path).unwrap();
        fs::write(
            path,
            original.replace("wiki_id: vault_transient", "wiki_id: vault_replaced"),
        )
        .unwrap();
        // This is an ordinary external edit to the public root marker. The old
        // Catalog and retained authority still agree, so only captured bytes can
        // reveal the changed identity. A tiny SQL allowance also proves refusal
        // precedes SQL construction rather than becoming a budget fallback.
        let error = match catalog.current_query_snapshot(QueryReadLimits {
            max_vm_steps: 1,
            ..Default::default()
        }) {
            Err(error) => error,
            Ok(_) => panic!("changed marker must not produce a reader or resource fallback"),
        };
        assert_eq!(error.code, ErrorCode::ContentConflict);
        assert_eq!(catalog.operation_state().unwrap().unwrap(), authority);
    }

    #[test]
    fn added_member_after_capture_invalidates_full_proof() {
        let (temp, catalog) = fixture();
        let reader = current(&catalog);
        fs::write(
            temp.path().join("late.md"),
            page("page_late", "late new fact"),
        )
        .unwrap();
        assert_eq!(
            reader.final_recheck(&catalog).unwrap_err().code,
            ErrorCode::ContentConflict
        );
    }

    #[test]
    fn idle_operation_that_begins_and_ends_still_invalidates_observation() {
        let (_temp, catalog) = fixture();
        let reader = current(&catalog);
        let initial = catalog.operation_state().unwrap().unwrap();
        let writer = WriterPermit::acquire(catalog.fs.root(), Duration::from_secs(1)).unwrap();
        let change = PreparedChange {
            change_id: RecordId::new("change_transient_race").unwrap(),
            manifest_hash: Blake3Hash::digest("race"),
        };
        let next = Publication {
            file_id: initial.publication().file_id.clone(),
            epoch: initial.publication().epoch + 1,
        };
        let active =
            operation_authority::begin(&catalog.fs, &writer, &initial, change.clone(), next)
                .unwrap();
        assert_eq!(
            reader.verify_operations(&catalog).unwrap_err().code,
            ErrorCode::RecoveryRequired
        );
        operation_authority::cancel(&catalog.fs, &writer, &active, &change).unwrap();
        assert_eq!(
            reader.final_recheck(&catalog).unwrap_err().code,
            ErrorCode::ContentConflict
        );
    }

    #[test]
    fn missing_corrupt_and_active_authority_are_never_budget_fallbacks() {
        for corruption in [None, Some("{}"), Some("active")] {
            let (temp, catalog) = fixture();
            if corruption == Some("active") {
                let initial = catalog.operation_state().unwrap().unwrap();
                let writer =
                    WriterPermit::acquire(catalog.fs.root(), Duration::from_secs(1)).unwrap();
                operation_authority::begin(
                    &catalog.fs,
                    &writer,
                    &initial,
                    PreparedChange {
                        change_id: RecordId::new("change_pending").unwrap(),
                        manifest_hash: Blake3Hash::digest("pending"),
                    },
                    Publication {
                        file_id: initial.publication().file_id.clone(),
                        epoch: initial.publication().epoch + 1,
                    },
                )
                .unwrap();
            } else {
                let path = temp.path().join(".wiki/state/operations.json");
                match corruption {
                    None => fs::remove_file(path).unwrap(),
                    Some(bytes) => fs::write(path, bytes).unwrap(),
                }
            }
            let error = match catalog.current_query_snapshot(QueryReadLimits {
                max_vm_steps: 1,
                ..Default::default()
            }) {
                Err(error) => error,
                Ok(_) => panic!("authority failure must be an error"),
            };
            assert_eq!(error.code, ErrorCode::RecoveryRequired);
        }
    }

    #[test]
    fn explicit_sql_and_input_bounds_return_only_bounded_unavailable() {
        let (temp, catalog) = fixture();
        assert!(matches!(
            catalog
                .current_query_snapshot(QueryReadLimits {
                    max_vm_steps: 1,
                    ..Default::default()
                })
                .unwrap(),
            CurrentQueryAttempt::BoundedUnavailable(_)
        ));
        // Preflight length admission must refuse this sparse file before loading.
        let file = fs::File::create(temp.path().join("too-large.md")).unwrap();
        file.set_len(8 * 1024 * 1024 + 1).unwrap();
        assert!(matches!(
            catalog
                .current_query_snapshot(QueryReadLimits::default())
                .unwrap(),
            CurrentQueryAttempt::BoundedUnavailable(_)
        ));
    }

    #[test]
    fn in_memory_page_ceiling_is_a_resource_refusal() {
        use crate::catalog::types::RetrievalSink;
        let (_temp, catalog) = fixture();
        let reader = current(&catalog);
        let mut document = reader
            .document(&VaultRelativePath::new("old.md").unwrap())
            .unwrap()
            .unwrap();
        document.body = "capacityprobe ".repeat(10_000);
        document.raw_text = document.body.clone();
        let connection = Connection::open_in_memory().unwrap();
        let limits = QueryReadLimits::default();
        let started = Instant::now();
        super::super::query::configure_query_at(&connection, &limits, started, true, None).unwrap();
        let mut rows = MemoryRows::new(connection, started, &limits).unwrap();
        let pages: i64 = rows
            .connection
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .unwrap();
        rows.connection
            .pragma_update(None, "max_page_count", pages + 1)
            .unwrap();
        assert_eq!(
            rows.document(document).unwrap_err().code,
            ErrorCode::BudgetExceeded
        );
    }

    #[test]
    fn selected_proof_and_final_recheck_share_one_input_allowance() {
        let (_temp, catalog) = fixture();
        let reader = current(&catalog);
        let observation = reader.current_observation().unwrap();
        let (bytes, _, _) = observation.remaining_work().unwrap();
        observation.charge_work(bytes, 1, 1).unwrap();
        assert_eq!(
            reader.final_recheck(&catalog).unwrap_err().code,
            ErrorCode::BudgetExceeded
        );
    }

    #[test]
    fn duplicate_identity_remains_ambiguous_and_never_yields_a_unique_claim() {
        let (temp, catalog) = fixture();
        fs::copy(temp.path().join("old.md"), temp.path().join("duplicate.md")).unwrap();
        let reader = current(&catalog);
        assert_eq!(
            reader
                .unique_identity_claim(&RecordId::new("page_old").unwrap())
                .unwrap_err()
                .code,
            ErrorCode::ReferenceAmbiguous
        );
        assert!(
            reader
                .document(&VaultRelativePath::new("old.md").unwrap())
                .unwrap()
                .is_some()
        );
        reader.final_recheck(&catalog).unwrap();
    }
}

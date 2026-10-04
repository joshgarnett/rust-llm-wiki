//! Explicit sync compares current input commitments, without a SQLite integrity audit.
use super::{
    Catalog, SyncReport, maintenance_input::MaintenanceInput, normalized_read, scan, selector, sql,
};
use crate::{
    domain::{Blake3Hash, ErrorCode, Result, VaultRelativePath, WikiError},
    sources::{SourceInputReads, revision::canonical_path},
    vault::ExpectedState,
};
use rusqlite::{Connection, OpenFlags, limits::Limit};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

pub(crate) fn unchanged(catalog: &Catalog, input: &MaintenanceInput) -> Result<Option<SyncReport>> {
    let authority = catalog.operation_state()?.ok_or_else(|| {
        WikiError::new(
            ErrorCode::RecoveryRequired,
            "sync requires normalized publication authority",
        )
    })?;
    authority.require_publication(authority.publication())?;
    let started = Instant::now();
    let ticks = Arc::new(AtomicU64::new(0));
    let selected = selector::acquire(
        catalog.fs(),
        catalog.vault_id(),
        Duration::from_millis(catalog.options.busy_timeout_ms),
        |path, selection| {
            let connection = Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_ONLY
                    | OpenFlags::SQLITE_OPEN_NO_MUTEX
                    | OpenFlags::SQLITE_OPEN_NOFOLLOW,
            )
            .map_err(sql::sql_error)?;
            sql::configure(&connection, catalog.options.busy_timeout_ms, false)?;
            connection
                .set_limit(Limit::SQLITE_LIMIT_LENGTH, 1024 * 1024)
                .map_err(sql::sql_error)?;
            connection
                .execute_batch("PRAGMA mmap_size=0; PRAGMA cache_size=-32768; BEGIN DEFERRED")
                .map_err(sql::sql_error)?;
            let ticks = ticks.clone();
            connection
                .progress_handler(
                    1000,
                    Some(move || {
                        started.elapsed() >= Duration::from_secs(30 * 60)
                            || ticks.fetch_add(1, Ordering::Relaxed) >= 1_000_000
                    }),
                )
                .map_err(sql::sql_error)?;
            let header = normalized_read::header(&connection, selection)?;
            authority.require_publication(&crate::changes::operation_authority::Publication {
                file_id: selection.file_id.clone(),
                epoch: header.snapshot.generation,
            })?;
            Ok((connection, header))
        },
    )?
    .ok_or_else(|| WikiError::new(ErrorCode::RecoveryRequired, "sync publication is missing"))?;
    let (connection, header) = selected.value();
    if header.snapshot.parser_fingerprint != scan::parser_fingerprint() {
        return Ok(None);
    }
    let (proof, ownership): (i64, i64) = connection.query_row(
        "SELECT proof_layout_version,revision_ownership_version FROM catalog_meta WHERE singleton=1", [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).map_err(sql::sql_error)?;
    if !(0..=2).contains(&proof) || !(0..=1).contains(&ownership) {
        return Err(WikiError::new(
            ErrorCode::IndexCorrupt,
            "unsupported maintenance layout version",
        ));
    }
    if proof != 2 || ownership != 1 {
        return Ok(None);
    }
    match super::normalized_audit::validate_schema(connection) {
        Ok(()) => {}
        Err(error) if error.code == ErrorCode::IndexCorrupt => return Ok(None),
        Err(error) => return Err(error),
    }
    // A current-epoch delta updates these rows, whereas the full-build audit
    // digest deliberately becomes historical after the first managed edit.
    let mut statement = connection
        .prepare("SELECT path,expected_hash FROM dependencies ORDER BY path")
        .map_err(sql::sql_error)?;
    let mut rows = statement.query([]).map_err(sql::sql_error)?;
    let mut count = 0usize;
    let mut canonical_count = 0usize;
    while let Some(row) = rows.next().map_err(sql::sql_error)? {
        input.require_clean()?;
        count += 1;
        if count > 2_000_000 {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "sync dependency row allowance exhausted",
            ));
        }
        let raw_path = row
            .get_ref(0)
            .map_err(sql::sql_error)?
            .as_str()
            .map_err(|e| WikiError::new(ErrorCode::IndexCorrupt, e.to_string()))?;
        if raw_path.len() > 64 * 1024 {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "sync dependency path exceeds allowance",
            ));
        }
        let path = VaultRelativePath::new(raw_path)?;
        let expected = match row.get_ref(1).map_err(sql::sql_error)? {
            rusqlite::types::ValueRef::Null => ExpectedState::Absent,
            rusqlite::types::ValueRef::Text(bytes) if bytes.len() <= 128 => {
                ExpectedState::Hash(Blake3Hash::new(
                    std::str::from_utf8(bytes)
                        .map_err(|e| WikiError::new(ErrorCode::IndexCorrupt, e.to_string()))?,
                )?)
            }
            _ => {
                return Err(WikiError::new(
                    ErrorCode::IndexCorrupt,
                    "sync dependency hash is invalid",
                ));
            }
        };
        if canonical_path(&path)
            && let ExpectedState::Hash(hash) = &expected
        {
            if input
                .notes()
                .get(&path)
                .is_none_or(|note| &note.source_hash != hash)
            {
                return Ok(None);
            }
            canonical_count += 1;
        } else if input.state_observed(&path)? != expected {
            return Ok(None);
        }
    }
    if canonical_count != input.notes().len() {
        return Ok(None);
    }
    input.final_recheck()?;
    if catalog.operation_state()?.as_ref() != Some(&authority) {
        return Err(WikiError::new(
            ErrorCode::RecoveryRequired,
            "sync publication authority changed",
        ));
    }
    Ok(Some(SyncReport {
        snapshot: header.snapshot.clone(),
        reused: true,
        vector_cache_lost: header.vector_cache_lost,
        vector_loss_unknown: header.vector_loss_unknown,
    }))
}

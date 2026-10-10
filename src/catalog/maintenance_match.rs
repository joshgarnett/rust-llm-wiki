//! Explicit sync compares current input commitments, without a SQLite integrity audit.
use super::{
    Catalog, SyncReport, maintenance_input::MaintenanceInput, normalized_read, scan, selector, sql,
};
use crate::{
    domain::{Blake3Hash, ErrorCode, Result, VaultRelativePath, WikiError},
    sources::revision::canonical_path,
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

pub(super) enum Comparison {
    Unchanged(SyncReport),
    Changed {
        base: crate::domain::ReadSnapshot,
        paths: std::collections::BTreeMap<VaultRelativePath, (ExpectedState, ExpectedState)>,
        page_only: bool,
    },
    Rebuild,
}

pub(super) fn compare(catalog: &Catalog, input: &MaintenanceInput) -> Result<Comparison> {
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
        return Ok(Comparison::Rebuild);
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
        return Ok(Comparison::Rebuild);
    }
    match super::normalized_audit::validate_schema(connection) {
        Ok(()) => {}
        Err(error) if error.code == ErrorCode::IndexCorrupt => return Ok(Comparison::Rebuild),
        Err(error) => return Err(error),
    }
    // A current-epoch delta updates these rows, whereas the full-build audit
    // digest deliberately becomes historical after the first managed edit.
    let mut statement = connection
        .prepare("SELECT path,expected_hash FROM dependencies ORDER BY path")
        .map_err(sql::sql_error)?;
    let mut rows = statement.query([]).map_err(sql::sql_error)?;
    let mut count = 0usize;
    let mut canonical_paths = std::collections::BTreeSet::new();
    let mut changed = std::collections::BTreeMap::new();
    let mut page_only = true;
    loop {
        // SQLite decoding and transaction ownership stay on the coordinator.
        // At most sixteen independent read descriptors/results are retained.
        let mut dependencies = Vec::new();
        let mut decode_error = None;
        for _ in 0..crate::maintenance_parallel::MAX_JOBS {
            let decoded = (|| -> Result<Option<(VaultRelativePath, ExpectedState)>> {
                let Some(row) = rows.next().map_err(sql::sql_error)? else {
                    return Ok(None);
                };
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
                        ExpectedState::Hash(Blake3Hash::new(std::str::from_utf8(bytes).map_err(
                            |e| WikiError::new(ErrorCode::IndexCorrupt, e.to_string()),
                        )?)?)
                    }
                    _ => {
                        return Err(WikiError::new(
                            ErrorCode::IndexCorrupt,
                            "sync dependency hash is invalid",
                        ));
                    }
                };
                Ok(Some((path, expected)))
            })();
            match decoded {
                Ok(Some(dependency)) => dependencies.push(dependency),
                Ok(None) => break,
                Err(error) => {
                    decode_error = Some(error);
                    break;
                }
            }
        }
        let paths: Vec<_> = dependencies
            .iter()
            .filter(|(path, _)| !canonical_path(path))
            .map(|(path, _)| path.clone())
            .collect();
        let mut observed = input.states_observed(&paths)?.into_iter();
        for (path, expected) in dependencies.iter() {
            let actual = if canonical_path(path) {
                canonical_paths.insert(path.clone());
                input
                    .notes()
                    .get(path)
                    .map_or(ExpectedState::Absent, |note| {
                        ExpectedState::Hash(note.source_hash.clone())
                    })
            } else {
                observed.next().expect("one observation per dependency")
            };
            if &actual != expected {
                if !canonical_path(path) {
                    page_only = false;
                }
                changed.insert(path.clone(), (expected.clone(), actual));
            }
        }
        // An earlier dependency failure wins over a later row decode failure.
        if let Some(error) = decode_error {
            return Err(error);
        }
        if dependencies.len() < crate::maintenance_parallel::MAX_JOBS {
            break;
        }
    }
    for (path, note) in input.notes().iter() {
        input.require_clean()?;
        if !canonical_paths.contains(path) {
            changed.insert(
                path.clone(),
                (
                    ExpectedState::Absent,
                    ExpectedState::Hash(note.source_hash.clone()),
                ),
            );
        }
    }
    if !changed.is_empty() {
        return Ok(Comparison::Changed {
            base: header.snapshot.clone(),
            paths: changed,
            page_only,
        });
    }
    input.final_recheck()?;
    if catalog.operation_state()?.as_ref() != Some(&authority) {
        return Err(WikiError::new(
            ErrorCode::RecoveryRequired,
            "sync publication authority changed",
        ));
    }
    Ok(Comparison::Unchanged(SyncReport {
        snapshot: header.snapshot.clone(),
        reused: true,
        vector_cache_lost: header.vector_cache_lost,
        vector_loss_unknown: header.vector_loss_unknown,
    }))
}

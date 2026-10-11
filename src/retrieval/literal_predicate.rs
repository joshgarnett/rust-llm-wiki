//! Query-scoped literal presence predicate; original-byte offset matching stays separate.
use memchr::memmem::Finder;
use rusqlite::{Connection, Error, Result, ffi, functions::FunctionFlags, types::ValueRef};
use std::time::Instant;

pub(super) const FUNCTION: &str = "lwiki_literal_present";

/// Register only for the lifetime of one normalized literal candidate query.
/// The caller must drop every statement using the function before this guard.
pub(super) fn register<'a>(
    connection: &'a Connection,
    query: &str,
    deadline: Option<Instant>,
) -> Result<Registration<'a>> {
    if query.is_empty() {
        return Err(Error::InvalidParameterName("empty literal query".into()));
    }
    // Do not replace another live registration, whose guard would then own ours.
    let occupied: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_function_list WHERE name=?1 AND narg=2)",
        [FUNCTION],
        |row| row.get(0),
    )?;
    if occupied {
        return Err(Error::InvalidParameterName(
            "literal predicate is already registered on this connection".into(),
        ));
    }
    let finder = Finder::new(query.as_bytes()).into_owned();
    // Deadline checks make this callback time-dependent, so it is not deterministic.
    connection.create_scalar_function(
        FUNCTION,
        2,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DIRECTONLY,
        move |context| {
            check_deadline(deadline)?;
            let needle = match context.get_raw(1) {
                ValueRef::Text(bytes) => bytes,
                value => return Err(Error::InvalidFunctionParameterType(1, value.data_type())),
            };
            if needle != finder.needle() {
                return Err(Error::InvalidParameterName(
                    "literal predicate query differs from its registration".into(),
                ));
            }
            let original = match context.get_raw(0) {
                ValueRef::Text(bytes) => bytes,
                value => return Err(Error::InvalidFunctionParameterType(0, value.data_type())),
            };
            let found = finder.find(original).is_some();
            check_deadline(deadline)?;
            Ok(found)
        },
    )?;
    Ok(Registration {
        connection,
        active: true,
    })
}

fn check_deadline(deadline: Option<Instant>) -> Result<()> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        // No message: rusqlite's error reporter must preserve SQLITE_INTERRUPT.
        return Err(Error::SqliteFailure(
            ffi::Error::new(ffi::SQLITE_INTERRUPT),
            None,
        ));
    }
    Ok(())
}

pub(super) struct Registration<'a> {
    connection: &'a Connection,
    active: bool,
}

impl Registration<'_> {
    /// On success, report removal errors after the caller has finalized statements.
    pub(super) fn finish(mut self) -> Result<()> {
        self.connection.remove_function(FUNCTION, 2)?;
        self.active = false;
        Ok(())
    }
}

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        if self.active {
            // Error paths preserve their original error. The query's statement
            // locals are declared after this guard and therefore drop first.
            let _ = self.connection.remove_function(FUNCTION, 2);
        }
    }
}

#[cfg(test)]
#[path = "literal_predicate_tests.rs"]
mod tests;

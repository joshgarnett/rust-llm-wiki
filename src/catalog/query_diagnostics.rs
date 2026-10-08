//! Test-only observations of the public coordinator; no production tracing API.
use crate::domain::Blake3Hash;
use rusqlite::{Row, types::ValueRef};
use serde::Serialize;
use std::{cell::RefCell, path::Path};

#[derive(Default, Serialize)]
pub(crate) struct Observation {
    pub reads: Vec<Read>,
    pub cache_rows: Vec<CacheRow>,
    pub vm_steps: u64,
    pub plans: Vec<(String, Vec<String>, u64)>,
}
#[derive(Serialize)]
pub(crate) struct Read {
    pub layer: &'static str,
    pub path: String,
    pub bytes: usize,
}
#[derive(Serialize)]
pub(crate) struct CacheRow {
    pub columns: Vec<String>,
    pub values: Vec<String>,
}
thread_local! {
    static ACTIVE: RefCell<Option<Observation>> = const { RefCell::new(None) };
    static BEFORE_FINAL: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
    static FORBIDDEN_ACCESS: RefCell<Option<AccessObservation>> = const { RefCell::new(None) };
}

struct AccessObservation {
    forbidden: Vec<&'static str>,
    attempted: Vec<&'static str>,
}

/// Observes named maintenance boundaries on the invoking thread, not OS syscalls.
pub(crate) struct ForbiddenAccessGuard {
    finished: bool,
    _thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}

pub(crate) fn forbid_access(boundaries: &[&'static str]) -> ForbiddenAccessGuard {
    FORBIDDEN_ACCESS.with(|slot| {
        let mut slot = slot.borrow_mut();
        assert!(slot.is_none(), "nested forbidden-access observation");
        *slot = Some(AccessObservation {
            forbidden: boundaries.to_vec(),
            attempted: Vec::new(),
        });
    });
    ForbiddenAccessGuard {
        finished: false,
        _thread_bound: std::marker::PhantomData,
    }
}

pub(crate) fn access(boundary: &'static str) {
    let forbidden = FORBIDDEN_ACCESS.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(observation) = slot.as_mut() else {
            return false;
        };
        observation.attempted.push(boundary);
        observation.forbidden.contains(&boundary)
    });
    // Release the RefCell borrow before unwinding through the observed code.
    assert!(!forbidden, "forbidden maintenance access: {boundary}");
}

impl ForbiddenAccessGuard {
    pub(crate) fn finish(mut self) -> Vec<&'static str> {
        let observation = FORBIDDEN_ACCESS.with(|slot| {
            slot.borrow_mut()
                .take()
                .expect("active forbidden-access observation")
        });
        self.finished = true;
        observation.attempted
    }
}

impl Drop for ForbiddenAccessGuard {
    fn drop(&mut self) {
        if !self.finished {
            FORBIDDEN_ACCESS.with(|slot| {
                slot.borrow_mut().take();
            });
        }
    }
}
pub(crate) fn begin() {
    ACTIVE.with(|slot| assert!(slot.borrow_mut().replace(Observation::default()).is_none()));
}
pub(crate) fn end() -> Observation {
    ACTIVE.with(|slot| slot.borrow_mut().take().expect("active query observer"))
}
pub(crate) fn read(layer: &'static str, path: &Path, bytes: usize) {
    ACTIVE.with(|slot| {
        if let Some(observation) = slot.borrow_mut().as_mut() {
            observation.reads.push(Read {
                layer,
                path: path.to_string_lossy().into(),
                bytes,
            });
        }
    });
}
pub(crate) fn vm_step() {
    ACTIVE.with(|slot| {
        if let Some(observation) = slot.borrow_mut().as_mut() {
            observation.vm_steps += 1;
        }
    });
}
pub(crate) fn row(row: &Row<'_>, columns: usize) {
    ACTIVE.with(|slot| {
        if let Some(observation) = slot.borrow_mut().as_mut() {
            let names = (0..columns)
                .map(|i| row.as_ref().column_name(i).unwrap_or("?").into())
                .collect();
            let values = (0..columns)
                .map(|i| match row.get_ref(i) {
                    Ok(ValueRef::Text(bytes) | ValueRef::Blob(bytes)) if bytes.len() <= 256 => {
                        String::from_utf8_lossy(bytes).into()
                    }
                    Ok(ValueRef::Text(bytes) | ValueRef::Blob(bytes)) => {
                        Blake3Hash::digest(bytes).to_string()
                    }
                    Ok(ValueRef::Integer(value)) => value.to_string(),
                    Ok(ValueRef::Real(value)) => value.to_string(),
                    Ok(ValueRef::Null) => "null".into(),
                    Err(_) => "invalid".into(),
                })
                .collect();
            observation.cache_rows.push(CacheRow {
                columns: names,
                values,
            });
        }
    });
}
pub(crate) fn plan(sql: &str, details: Vec<String>, vm_steps: u64) {
    ACTIVE.with(|slot| {
        if let Some(observation) = slot.borrow_mut().as_mut() {
            observation.plans.push((sql.into(), details, vm_steps));
        }
    });
}
pub(crate) fn active() -> bool {
    ACTIVE.with(|slot| slot.borrow().is_some())
}
pub(crate) fn vm_count() -> u64 {
    ACTIVE.with(|slot| slot.borrow().as_ref().map_or(0, |value| value.vm_steps))
}
pub(crate) fn on_before_final(callback: impl FnOnce() + 'static) {
    BEFORE_FINAL.with(|slot| assert!(slot.borrow_mut().replace(Box::new(callback)).is_none()));
}
pub(crate) fn before_final() {
    let callback = BEFORE_FINAL.with(|slot| slot.borrow_mut().take());
    if let Some(callback) = callback {
        callback();
    }
}
pub(crate) fn clear_before_final() {
    BEFORE_FINAL.with(|slot| {
        slot.borrow_mut().take();
    });
}

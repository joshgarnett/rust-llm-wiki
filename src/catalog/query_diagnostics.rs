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
    pub trace_overflow: bool,
    #[serde(skip)]
    worker: bool,
    #[serde(skip)]
    owned_bytes: usize,
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
    overflow: bool,
    worker: bool,
}

#[derive(Clone)]
pub(crate) struct WorkerContext {
    reads: bool,
    forbidden: Option<Vec<&'static str>>,
}
pub(crate) struct WorkerDelta {
    reads: Option<Observation>,
    access: Option<AccessObservation>,
}
impl WorkerContext {
    pub(crate) fn capture() -> Self {
        Self {
            reads: active(),
            forbidden: FORBIDDEN_ACCESS
                .with(|slot| slot.borrow().as_ref().map(|v| v.forbidden.clone())),
        }
    }
    pub(crate) fn reservation_bytes(&self) -> u64 {
        if self.reads || self.forbidden.is_some() {
            1024 * 1024
        } else {
            0
        }
    }
    pub(crate) fn install(&self) {
        if self.reads {
            begin();
            ACTIVE.with(|slot| slot.borrow_mut().as_mut().unwrap().worker = true);
        }
        if let Some(forbidden) = &self.forbidden {
            FORBIDDEN_ACCESS.with(|slot| {
                assert!(
                    slot.borrow().is_none(),
                    "worker access observer already active"
                );
                *slot.borrow_mut() = Some(AccessObservation {
                    forbidden: forbidden.clone(),
                    attempted: Vec::new(),
                    overflow: false,
                    worker: true,
                });
            });
        }
    }
    pub(crate) fn take(&self) -> WorkerDelta {
        WorkerDelta {
            reads: self.reads.then(end),
            access: self.forbidden.as_ref().map(|_| {
                FORBIDDEN_ACCESS.with(|slot| {
                    slot.borrow_mut()
                        .take()
                        .expect("worker access observer missing")
                })
            }),
        }
    }
}
impl WorkerDelta {
    pub(crate) fn merge(self) -> crate::domain::Result<()> {
        let mut overflow = false;
        if let Some(mut delta) = self.reads {
            overflow |= delta.trace_overflow;
            ACTIVE.with(|slot| {
                let mut slot = slot.borrow_mut();
                let owner = slot
                    .as_mut()
                    .expect("owner query observer ended before worker join");
                for read in delta.reads.drain(..) {
                    append_read(owner, read);
                }
                owner.trace_overflow |= delta.trace_overflow;
                owner.vm_steps += delta.vm_steps;
                // SQL observations are owner-only; a worker attempting one marks overflow.
                owner.trace_overflow |= !delta.cache_rows.is_empty() || !delta.plans.is_empty();
                overflow |= owner.trace_overflow;
            });
        }
        if let Some(delta) = self.access {
            overflow |= delta.overflow;
            FORBIDDEN_ACCESS.with(|slot| {
                let mut slot = slot.borrow_mut();
                let owner = slot
                    .as_mut()
                    .expect("owner access observer ended before worker join");
                for boundary in delta.attempted {
                    append_access(owner, boundary);
                }
                owner.overflow |= delta.overflow;
                overflow |= owner.overflow;
            });
        }
        if overflow {
            Err(crate::domain::WikiError::new(
                crate::domain::ErrorCode::BudgetExceeded,
                "diagnostic trace overflow; returned observations are incomplete",
            ))
        } else {
            Ok(())
        }
    }
}
// The remaining128KiB of the1MiB reservation covers access lists and finite profiles.
const WORKER_TRACE_BYTES: usize = 896 * 1024;
// Reserve the remaining1MiB for the separately owned access descriptor/trace.
const OWNER_TRACE_BYTES: usize = 63 * 1024 * 1024;
fn append_read(observation: &mut Observation, read: Read) {
    let limit = if observation.worker {
        WORKER_TRACE_BYTES
    } else {
        OWNER_TRACE_BYTES
    };
    if observation.trace_overflow
        || (observation.worker
            && observation.reads.len()
                + FORBIDDEN_ACCESS.with(|slot| {
                    slot.borrow()
                        .as_ref()
                        .map_or(0, |value| value.attempted.len())
                })
                >= 1024)
    {
        observation.trace_overflow = true;
        return;
    }
    let old = observation.reads.capacity();
    let next = if old == observation.reads.len() {
        old.saturating_mul(2).max(16)
    } else {
        old
    };
    let added = next.saturating_sub(old) * std::mem::size_of::<Read>() + read.path.capacity();
    if observation.owned_bytes.saturating_add(added) > limit {
        observation.trace_overflow = true;
        return;
    }
    if next > old
        && observation
            .reads
            .try_reserve_exact(next - observation.reads.len())
            .is_err()
    {
        observation.trace_overflow = true;
        return;
    }
    let actual =
        (observation.reads.capacity() - old) * std::mem::size_of::<Read>() + read.path.capacity();
    if observation.owned_bytes.saturating_add(actual) > limit {
        observation.trace_overflow = true;
        return;
    }
    observation.owned_bytes += actual;
    observation.reads.push(read);
}
fn append_access(observation: &mut AccessObservation, boundary: &'static str) {
    let maximum = if observation.worker {
        1024
    } else {
        (1024 * 1024 / std::mem::size_of::<&str>()) - 1024
    };
    if observation.overflow
        || observation.attempted.len() >= maximum
        || (observation.worker
            && observation.attempted.len()
                + ACTIVE.with(|slot| slot.borrow().as_ref().map_or(0, |value| value.reads.len()))
                >= 1024)
    {
        observation.overflow = true;
        return;
    }
    if observation.attempted.len() == observation.attempted.capacity() {
        let next = observation
            .attempted
            .capacity()
            .saturating_mul(2)
            .max(16)
            .min(maximum);
        if observation
            .attempted
            .try_reserve_exact(next - observation.attempted.len())
            .is_err()
            || observation.attempted.capacity() > maximum
        {
            observation.overflow = true;
            return;
        }
    }
    observation.attempted.push(boundary);
}

/// Observes named maintenance boundaries on the invoking thread, not OS syscalls.
pub(crate) struct ForbiddenAccessGuard {
    finished: bool,
    _thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}

pub(crate) fn forbid_access(boundaries: &[&'static str]) -> ForbiddenAccessGuard {
    assert!(
        boundaries.len() <= 1024,
        "forbidden-access descriptor bound"
    );
    FORBIDDEN_ACCESS.with(|slot| {
        let mut slot = slot.borrow_mut();
        assert!(slot.is_none(), "nested forbidden-access observation");
        *slot = Some(AccessObservation {
            forbidden: boundaries.to_vec(),
            attempted: Vec::new(),
            overflow: false,
            worker: false,
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
        append_access(observation, boundary);
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
        assert!(
            !observation.overflow,
            "owner forbidden-access trace overflow"
        );
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
    let observation = ACTIVE.with(|slot| slot.borrow_mut().take().expect("active query observer"));
    assert!(
        observation.worker || !observation.trace_overflow,
        "owner query trace overflow"
    );
    observation
}
pub(crate) fn read(layer: &'static str, path: &Path, bytes: usize) {
    ACTIVE.with(|slot| {
        if let Some(observation) = slot.borrow_mut().as_mut() {
            let limit = if observation.worker {
                WORKER_TRACE_BYTES
            } else {
                OWNER_TRACE_BYTES
            };
            // Admit worst-case lossy UTF-8 expansion before materializing the string.
            if observation.trace_overflow
                || path
                    .as_os_str()
                    .len()
                    .saturating_mul(3)
                    .saturating_add(observation.owned_bytes)
                    > limit
            {
                observation.trace_overflow = true;
                return;
            }
            append_read(
                observation,
                Read {
                    layer,
                    path: path.to_string_lossy().into(),
                    bytes,
                },
            );
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
            if observation.worker {
                observation.trace_overflow = true;
                return;
            }
            let names: Vec<String> = (0..columns)
                .map(|i| row.as_ref().column_name(i).unwrap_or("?").into())
                .collect();
            let values: Vec<String> = (0..columns)
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
            let payload = (names.capacity() + values.capacity()) * std::mem::size_of::<String>()
                + names
                    .iter()
                    .chain(&values)
                    .map(String::capacity)
                    .sum::<usize>();
            let old = observation.cache_rows.capacity();
            let growth =
                usize::from(old == observation.cache_rows.len()) * std::mem::size_of::<CacheRow>();
            if observation.trace_overflow
                || observation
                    .owned_bytes
                    .saturating_add(payload)
                    .saturating_add(growth)
                    > OWNER_TRACE_BYTES
            {
                observation.trace_overflow = true;
                return;
            }
            if growth != 0 && observation.cache_rows.try_reserve_exact(1).is_err() {
                observation.trace_overflow = true;
                return;
            }
            let actual = payload
                + (observation.cache_rows.capacity() - old) * std::mem::size_of::<CacheRow>();
            if observation.owned_bytes.saturating_add(actual) > OWNER_TRACE_BYTES {
                observation.trace_overflow = true;
                return;
            }
            observation.owned_bytes += actual;
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
            if observation.worker {
                observation.trace_overflow = true;
                return;
            }
            let payload = sql.len()
                + details.capacity() * std::mem::size_of::<String>()
                + details.iter().map(String::capacity).sum::<usize>();
            let old = observation.plans.capacity();
            let growth = usize::from(old == observation.plans.len())
                * std::mem::size_of::<(String, Vec<String>, u64)>();
            if observation.trace_overflow
                || observation
                    .owned_bytes
                    .saturating_add(payload)
                    .saturating_add(growth)
                    > OWNER_TRACE_BYTES
            {
                observation.trace_overflow = true;
                return;
            }
            if growth != 0 && observation.plans.try_reserve_exact(1).is_err() {
                observation.trace_overflow = true;
                return;
            }
            let sql = sql.to_owned();
            let actual = payload - sql.len()
                + sql.capacity()
                + (observation.plans.capacity() - old)
                    * std::mem::size_of::<(String, Vec<String>, u64)>();
            if observation.owned_bytes.saturating_add(actual) > OWNER_TRACE_BYTES {
                observation.trace_overflow = true;
                return;
            }
            observation.owned_bytes += actual;
            observation.plans.push((sql, details, vm_steps));
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

#[test]
fn worker_trace_overflow_is_joined_and_marks_owner_evidence_invalid() {
    for mode in 0..3 {
        begin();
        let context = WorkerContext::capture();
        assert_eq!(context.reservation_bytes(), 1024 * 1024);
        let delta = std::thread::spawn(move || {
            context.install();
            match mode {
                0 => {
                    for _ in 0..1025 {
                        read("fixture", Path::new("bounded.md"), 1);
                    }
                }
                1 => read("fixture", Path::new(&"x".repeat(350_000)), 1),
                _ => plan("worker SQL trace forbidden", Vec::new(), 0),
            }
            context.take()
        })
        .join()
        .unwrap();
        let error = delta.merge().unwrap_err();
        assert_eq!(error.code, crate::domain::ErrorCode::BudgetExceeded);
        // end clears state before refusing to present an overflowed trace as valid.
        assert!(std::panic::catch_unwind(end).is_err());
    }
}

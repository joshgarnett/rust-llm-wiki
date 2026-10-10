//! Bounded scanned notes and lazy observed assets for explicit maintenance.
//! Admission counters are logical work/bytes, not a native RSS guarantee.
use super::maintenance_types::{MaintenanceFile, MaintenanceLimits, MaintenanceUsage};
use crate::maintenance_parallel::{self, BatchResults, Job};
use crate::{
    domain::{Blake3Hash, ErrorCode, RecordId, Result, VaultRelativePath, WikiError},
    records::parse_note,
    sources::{SourceInputReads, SourceNotes, revision::canonical_path},
    storage::layout::ValidatedLayout,
    vault::{ExpectedState, VaultFs},
};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs,
    io::Read,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Instant,
};

type ReadValue = (Observation, Option<Vec<u8>>);
#[derive(Default)]
struct ReadWork {
    bytes: AtomicU64,
    paths: AtomicUsize,
}
struct SharedBudget {
    io: AtomicU64,
    paths: AtomicUsize,
    retained: AtomicU64,
    exhausted: AtomicBool,
}
impl SharedBudget {
    fn take(&self, counter: &AtomicU64, amount: u64) -> Result<()> {
        counter
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
                left.checked_sub(amount)
            })
            .map_err(|_| {
                self.exhausted.store(true, Ordering::Relaxed);
                budget("maintenance batch byte allowance exhausted")
            })?;
        Ok(())
    }
    fn step(&self, work: &ReadWork) -> Result<()> {
        self.paths
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
                left.checked_sub(1)
            })
            .map_err(|_| {
                self.exhausted.store(true, Ordering::Relaxed);
                budget("maintenance path work allowance exhausted")
            })?;
        work.paths.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}
struct ReadResults {
    values: std::vec::IntoIter<Result<ReadValue>>,
    // Keep the executor memory lease until every retained result is reduced.
    _lease: BatchResults<ReadValue>,
}
impl Iterator for ReadResults {
    type Item = Result<ReadValue>;
    fn next(&mut self) -> Option<Self::Item> {
        self.values.next()
    }
}
#[cfg(test)]
struct ReadDiagnostic {
    path: std::path::PathBuf,
    work: Arc<ReadWork>,
}
#[cfg(test)]
impl Drop for ReadDiagnostic {
    fn drop(&mut self) {
        crate::catalog::query_diagnostics::read(
            "maintenance",
            &self.path,
            self.work.bytes.load(Ordering::Relaxed) as usize,
        );
    }
}
struct ReadDescriptor {
    fs: Arc<VaultFs>,
    physical: VaultRelativePath,
    retain: bool,
    ceiling: usize,
    deadline: Instant,
    budget: Arc<SharedBudget>,
    work: Arc<ReadWork>,
}
impl ReadDescriptor {
    fn clean(&self) -> Result<()> {
        if Instant::now() >= self.deadline {
            return Err(budget("maintenance input deadline exhausted"));
        }
        Ok(())
    }
    fn read(self) -> Result<ReadValue> {
        #[cfg(test)]
        parallel_tests::hook("before_observation", &self.fs, &self.physical);
        self.clean()?;
        let resolve = || {
            self.clean()?;
            self.fs
                .root()
                .resolve_raw_budgeted(&self.physical, &mut || {
                    self.clean()?;
                    self.budget.step(&self.work)
                })
        };
        let absolute = resolve()?;
        #[cfg(test)]
        let _diagnostic = ReadDiagnostic {
            path: absolute.clone(),
            work: self.work.clone(),
        };
        let before = match fs::symlink_metadata(&absolute) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((
                    Observation {
                        expected: ExpectedState::Absent,
                        bytes: 0,
                    },
                    None,
                ));
            }
            Err(error) => return Err(io(error)),
        };
        if !before.is_file() {
            return Err(conflict("maintenance input is not a regular file"));
        }
        if before.len() > self.ceiling as u64 {
            return Err(budget(
                "maintenance input exceeds byte allowance before allocation",
            ));
        }
        #[cfg(test)]
        parallel_tests::hook("before_open", &self.fs, &self.physical);
        #[cfg(unix)]
        let mut file = {
            use std::os::unix::fs::OpenOptionsExt;
            fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&absolute)
                .map_err(io)?
        };
        #[cfg(not(unix))]
        let mut file = fs::File::open(&absolute).map_err(io)?;
        let opened = file.metadata().map_err(io)?;
        MaintenanceInput::same_file(&before, &opened)?;
        if opened.len() > self.ceiling as u64 {
            return Err(budget(
                "maintenance opened input exceeds byte allowance before allocation",
            ));
        }
        // Whole-file reservations do not strand a final short read behind
        // another job's oversized read buffer. No payload allocation precedes them.
        self.budget.take(&self.budget.io, opened.len())?;
        if self.retain {
            self.budget.take(&self.budget.retained, opened.len())?;
        }
        let mut output = self.retain.then(Vec::new);
        if let Some(bytes) = &mut output {
            bytes
                .try_reserve_exact(
                    usize::try_from(opened.len())
                        .map_err(|_| budget("maintenance file length conversion"))?,
                )
                .map_err(|_| budget("maintenance payload allocation failed"))?;
            if bytes.capacity() > self.ceiling {
                return Err(budget("maintenance payload capacity exceeds reservation"));
            }
        }
        let mut hash = blake3::Hasher::new();
        let mut length = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        let result = (|| {
            // Read exactly the reserved length. A changed length fails the
            // post-read guard, without consuming unadmitted growth bytes.
            while length < opened.len() {
                self.clean()?;
                let cap = (opened.len() - length).min(buffer.len() as u64) as usize;
                let count = file.read(&mut buffer[..cap]).map_err(io)?;
                if count == 0 {
                    break;
                }
                self.work.bytes.fetch_add(count as u64, Ordering::Relaxed);
                length += count as u64;
                hash.update(&buffer[..count]);
                if let Some(bytes) = &mut output {
                    bytes.extend_from_slice(&buffer[..count]);
                }
            }
            #[cfg(test)]
            parallel_tests::hook("after_bytes", &self.fs, &self.physical);
            let after = file.metadata().map_err(io)?;
            MaintenanceInput::same_file(&opened, &after)?;
            if resolve()? != absolute {
                return Err(conflict("maintenance input route changed while reading"));
            }
            let named = fs::symlink_metadata(&absolute).map_err(io)?;
            MaintenanceInput::same_file(&opened, &named)?;
            if after.len() != length || opened.len() != length {
                return Err(conflict("maintenance file length changed while reading"));
            }
            self.clean()?;
            Ok((
                Observation {
                    expected: ExpectedState::Hash(Blake3Hash::new(format!(
                        "blake3:{}",
                        hash.finalize().to_hex()
                    ))?),
                    bytes: length,
                },
                output,
            ))
        })();
        result
    }
}

#[derive(Clone)]
struct Observation {
    expected: ExpectedState,
    bytes: u64,
}
#[derive(Default)]
struct Mutable {
    usage: MaintenanceUsage,
    observed: BTreeMap<VaultRelativePath, Observation>,
    layout_observed: BTreeMap<VaultRelativePath, Observation>,
    fatal: Option<WikiError>,
}
pub(crate) struct MaintenanceInput {
    fs: VaultFs,
    vault_id: RecordId,
    notes: SourceNotes,
    scanned: BTreeMap<VaultRelativePath, MaintenanceFile>,
    limits: MaintenanceLimits,
    started: Instant,
    layout: Option<ValidatedLayout>,
    mutable: RefCell<Mutable>,
}
fn budget(message: &str) -> WikiError {
    WikiError::new(ErrorCode::BudgetExceeded, message)
}
fn conflict(message: &str) -> WikiError {
    WikiError::new(ErrorCode::ContentConflict, message)
}
fn io(error: std::io::Error) -> WikiError {
    WikiError::new(
        ErrorCode::Internal,
        format!("maintenance input read: {error}"),
    )
}

impl MaintenanceInput {
    pub(crate) fn capture(
        fs: &VaultFs,
        vault_id: &RecordId,
        limits: MaintenanceLimits,
    ) -> Result<Self> {
        let started = Instant::now();
        if limits.max_files == 0
            || limits.max_path_steps == 0
            || limits.max_manifest_bytes == 0
            || limits.max_file_bytes == 0
            || limits.max_file_bytes > 64 * 1024 * 1024
            || limits.max_retained_note_bytes == 0
            || limits.max_io_bytes == 0
            || limits.max_elapsed.is_zero()
            || started.checked_add(limits.max_elapsed).is_none()
        {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "invalid finite maintenance input limits",
            ));
        }
        let mut input = Self {
            fs: fs.clone(),
            vault_id: vault_id.clone(),
            notes: BTreeMap::new().into(),
            scanned: BTreeMap::new(),
            limits,
            started,
            layout: None,
            mutable: RefCell::new(Mutable::default()),
        };
        input.layout = Some(ValidatedLayout::capture_with_reader_budgeted(
            input.fs.root(),
            &mut |path, max| input.observe_layout(path, true, max),
            &mut || input.path_step(),
        )?);
        input.capture_notes()
    }
    fn capture_notes(mut self) -> Result<Self> {
        let input = &mut self;
        let paths = input.census(true)?;
        let mut notes = BTreeMap::new();
        let mut offset = 0;
        while offset < paths.len() {
            let (count, results) = input.guarded(|| {
                input.read_batch(
                    &paths[offset..],
                    true,
                    input.limits.max_file_bytes,
                    false,
                    true,
                )
            })?;
            for (path, result) in paths[offset..offset + count].iter().cloned().zip(results) {
                let (observation, bytes) = result.map_err(|error| input.latch(error))?;
                let layout_changed = input
                    .mutable
                    .borrow()
                    .layout_observed
                    .get(&path)
                    .is_some_and(|layout_state| {
                        layout_state.expected != observation.expected
                            || layout_state.bytes != observation.bytes
                    });
                if layout_changed {
                    return Err(input.latch(conflict(
                        "maintenance vault marker changed after layout validation",
                    )));
                }
                let ExpectedState::Hash(hash) = observation.expected else {
                    return Err(input.latch(conflict("scanned canonical path disappeared")));
                };
                input.scanned.insert(
                    path.clone(),
                    MaintenanceFile {
                        hash,
                        bytes: observation.bytes,
                    },
                );
                if let Some(bytes) = bytes {
                    let parsed = parse_note(&bytes);
                    drop(bytes);
                    let mut state = input.mutable.borrow_mut();
                    state.usage.retained_note_bytes = state
                        .usage
                        .retained_note_bytes
                        .checked_add(parsed.raw.len())
                        .filter(|n| *n <= input.limits.max_retained_note_bytes)
                        .ok_or_else(|| budget("maintenance retained notes exceed allowance"))?;
                    drop(state);
                    notes.insert(path, parsed);
                }
            }
            offset += count;
        }
        input.notes = notes.into();
        input.require_clean()?;
        Ok(self)
    }
    pub(crate) fn fs(&self) -> &VaultFs {
        &self.fs
    }
    pub(crate) fn vault_id(&self) -> &RecordId {
        &self.vault_id
    }
    pub(crate) fn notes(&self) -> &SourceNotes {
        &self.notes
    }
    pub(crate) fn usage(&self) -> MaintenanceUsage {
        self.mutable.borrow().usage.clone()
    }
    pub(crate) fn remaining_time(&self) -> Result<std::time::Duration> {
        self.require_clean()?;
        let remaining = self
            .limits
            .max_elapsed
            .saturating_sub(self.started.elapsed());
        if remaining.is_zero() {
            return Err(self.latch(budget("maintenance input deadline exhausted")));
        }
        Ok(remaining)
    }
    fn latch(&self, error: WikiError) -> WikiError {
        let mut state = self.mutable.borrow_mut();
        state.fatal.get_or_insert(error).clone()
    }
    pub(crate) fn require_clean(&self) -> Result<()> {
        if let Some(error) = self.mutable.borrow().fatal.clone() {
            return Err(error);
        }
        if self.started.elapsed() >= self.limits.max_elapsed {
            return Err(self.latch(budget("maintenance input deadline exhausted")));
        }
        Ok(())
    }
    fn guarded<T>(&self, run: impl FnOnce() -> Result<T>) -> Result<T> {
        self.require_clean()?;
        run().map_err(|error| self.latch(error))
    }
    fn path_step(&self) -> Result<()> {
        self.require_clean()?;
        let mut state = self.mutable.borrow_mut();
        state.usage.path_steps = state
            .usage
            .path_steps
            .checked_add(1)
            .filter(|n| *n <= self.limits.max_path_steps)
            .ok_or_else(|| budget("maintenance path work allowance exhausted"))?;
        Ok(())
    }
    fn admit_path(&self, path: &VaultRelativePath, new_state: bool) -> Result<()> {
        self.require_clean()?;
        // Conservative logical entry/path-list/map charge, before scanner push.
        // Recheck path-list charges are cumulative, even after that list is freed.
        let charge = path
            .as_str()
            .len()
            .checked_mul(3)
            .and_then(|n| n.checked_add(128))
            .ok_or_else(|| budget("maintenance path charge overflow"))?;
        let mut state = self.mutable.borrow_mut();
        let bytes = state
            .usage
            .manifest_bytes
            .checked_add(charge)
            .filter(|n| *n <= self.limits.max_manifest_bytes)
            .ok_or_else(|| budget("maintenance manifest allowance exhausted"))?;
        let files = state
            .usage
            .files
            .checked_add(usize::from(new_state))
            .filter(|n| *n <= self.limits.max_files)
            .ok_or_else(|| budget("maintenance file-state allowance exhausted"))?;
        state.usage.manifest_bytes = bytes;
        state.usage.files = files;
        Ok(())
    }
    fn census(&self, initial: bool) -> Result<Vec<VaultRelativePath>> {
        self.guarded(|| {
            self.fs.root().scan_markdown_with_layout(
                self.limits.max_files,
                &mut || self.path_step(),
                &mut |path| self.admit_path(path, initial),
                self.layout.as_ref().expect("validated maintenance layout"),
            )
        })
    }
    fn read_batch(
        &self,
        paths: &[VaultRelativePath],
        retain: bool,
        ceiling: usize,
        raw: bool,
        capture: bool,
    ) -> Result<(usize, ReadResults)> {
        self.require_clean()?;
        let usage = self.usage();
        let shared = Arc::new(SharedBudget {
            io: AtomicU64::new(self.limits.max_io_bytes.saturating_sub(usage.io_bytes)),
            paths: AtomicUsize::new(self.limits.max_path_steps.saturating_sub(usage.path_steps)),
            retained: AtomicU64::new(if capture {
                self.limits
                    .max_retained_note_bytes
                    .saturating_sub(usage.retained_note_bytes) as u64
            } else {
                u64::MAX
            }),
            exhausted: AtomicBool::new(false),
        });
        // The root binding is cloned once per phase, shared across descriptors.
        let worker_fs = Arc::new(self.fs.clone());
        let mut workspaces = Vec::with_capacity(maintenance_parallel::MAX_JOBS);
        // Decide for the whole prospective batch before any job is dispatched.
        // Unsupported pre-existing paths retain the owner observation route.
        for path in paths.iter().take(maintenance_parallel::MAX_JOBS) {
            let physical = if raw {
                path.clone()
            } else {
                self.layout
                    .as_ref()
                    .expect("validated maintenance layout")
                    .map(self.fs.root(), path)?
            };
            let workspace = crate::vault::paths::parallel_path_workspace(worker_fs.root(), path)
                .and_then(|logical| {
                    crate::vault::paths::parallel_path_workspace(worker_fs.root(), &physical)
                        .map(|physical| logical.max(physical))
                });
            workspaces.push(workspace);
        }
        let owner_sequential = workspaces.iter().any(Option::is_none);
        let mut jobs = Vec::with_capacity(maintenance_parallel::MAX_JOBS);
        let mut counters = Vec::with_capacity(maintenance_parallel::MAX_JOBS);
        let mut values = Vec::with_capacity(maintenance_parallel::MAX_JOBS);
        let descriptor_arrays = jobs.capacity() * std::mem::size_of::<Job<ReadValue>>()
            + counters.capacity() * std::mem::size_of::<Arc<ReadWork>>()
            + values.capacity() * std::mem::size_of::<Result<ReadValue>>()
            + workspaces.capacity() * std::mem::size_of::<Option<u64>>();
        let shared_fixed = descriptor_arrays
            + worker_fs.owned_bytes()
            + std::mem::size_of::<SharedBudget>()
            + 4 * std::mem::size_of::<usize>(); // two Arc control blocks
        let mut reserved = 0u64;
        if std::mem::size_of::<blake3::Hasher>() > 8192 {
            return Err(budget("maintenance hasher exceeds admitted workspace"));
        }
        for (index, path) in paths
            .iter()
            .take(maintenance_parallel::MAX_JOBS)
            .enumerate()
        {
            let retain = retain && (!capture || canonical_path(path));
            let physical = if raw {
                path.clone()
            } else {
                self.layout
                    .as_ref()
                    .expect("validated maintenance layout")
                    .map(self.fs.root(), path)?
            };
            // Exposed owned capacities and the root-approved finite resolver
            // workspace are admitted separately from opaque runtime overhead.
            let payload = if retain {
                (1 + u64::from(capture)) * ceiling as u64
            } else {
                0
            };
            let descriptor = std::mem::size_of::<ReadDescriptor>()
                + std::mem::size_of::<ReadWork>()
                + 2 * std::mem::size_of::<usize>(); // work Arc control block
            let first = if jobs.is_empty() {
                shared_fixed as u64
            } else {
                0
            };
            let reservation = payload
                + 64 * 1024
                + std::mem::size_of::<blake3::Hasher>() as u64
                + descriptor as u64
                + first
                + physical.owned_capacity() as u64
                + if owner_sequential {
                    0
                } else {
                    workspaces[index].expect("whole batch path eligibility")
                };
            if reserved + reservation > maintenance_parallel::MAX_IN_FLIGHT_BYTES {
                break;
            }
            reserved += reservation;
            let work = Arc::new(ReadWork::default());
            counters.push(work.clone());
            let descriptor = ReadDescriptor {
                fs: worker_fs.clone(),
                physical,
                retain,
                ceiling,
                deadline: self
                    .started
                    .checked_add(self.limits.max_elapsed)
                    .expect("validated deadline"),
                budget: shared.clone(),
                work,
            };
            jobs.push(Job::new(reservation, move || descriptor.read()));
        }
        let count = jobs.len();
        if count == 0 {
            return Err(self.latch(budget("maintenance read memory allowance exhausted")));
        }
        let batch = if owner_sequential {
            maintenance_parallel::run_batch_sequential(jobs)?
        } else {
            maintenance_parallel::run_batch(jobs)?
        };
        let mut lease = batch.into_iter();
        values.extend(lease.by_ref());
        // All admitted work, including errors/panics after an earlier failure,
        // is merged before ordered reduction can return to the caller.
        {
            let mut state = self.mutable.borrow_mut();
            for work in counters {
                let bytes = work.bytes.load(Ordering::Relaxed);
                state.usage.io_bytes += bytes;
                if raw {
                    state.usage.layout_io_bytes += bytes;
                }
                state.usage.path_steps += work.paths.load(Ordering::Relaxed);
            }
        }
        if shared.exhausted.load(Ordering::Relaxed) {
            return Err(self.latch(budget("maintenance checkpoint allowance exhausted")));
        }
        self.require_clean()?;
        Ok((
            count,
            ReadResults {
                values: values.into_iter(),
                _lease: lease,
            },
        ))
    }
    fn read_file(
        &self,
        path: &VaultRelativePath,
        retain: bool,
        ceiling: usize,
        raw: bool,
    ) -> Result<ReadValue> {
        let (_, mut results) =
            self.read_batch(std::slice::from_ref(path), retain, ceiling, raw, false)?;
        results.next().expect("single admitted read")
    }
    fn same_file(before: &fs::Metadata, after: &fs::Metadata) -> Result<()> {
        if !after.is_file() {
            return Err(conflict("maintenance file binding changed"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if before.dev() != after.dev() || before.ino() != after.ino() {
                return Err(conflict("maintenance file identity changed"));
            }
        }
        #[cfg(not(unix))]
        let _ = before;
        Ok(())
    }
    fn observe(
        &self,
        path: &VaultRelativePath,
        retain: bool,
        ceiling: usize,
    ) -> Result<Option<Vec<u8>>> {
        let expected = self
            .scanned
            .get(path)
            .map(|file| Observation {
                expected: ExpectedState::Hash(file.hash.clone()),
                bytes: file.bytes,
            })
            .or_else(|| self.mutable.borrow().observed.get(path).cloned());
        if expected.is_none() {
            self.admit_path(path, true)?;
        }
        let (actual, bytes) = self.read_file(path, retain, ceiling, false)?;
        self.reduce_observation(path, expected, actual)?;
        Ok(bytes)
    }
    fn reduce_observation(
        &self,
        path: &VaultRelativePath,
        expected: Option<Observation>,
        actual: Observation,
    ) -> Result<()> {
        if let Some(expected) = expected {
            if actual.expected != expected.expected || actual.bytes != expected.bytes {
                return Err(conflict("maintenance observed input changed"));
            }
        } else {
            let mut state = self.mutable.borrow_mut();
            state.observed.insert(path.clone(), actual);
            state.usage.observed_assets = state
                .usage
                .observed_assets
                .checked_add(1)
                .ok_or_else(|| budget("maintenance observation counter overflow"))?;
        }
        Ok(())
    }
    pub(crate) fn states_observed(
        &self,
        paths: &[VaultRelativePath],
    ) -> Result<Vec<ExpectedState>> {
        self.guarded(|| {
            if paths.len() > maintenance_parallel::MAX_JOBS {
                return Err(budget("maintenance dependency batch exceeds allowance"));
            }
            // Admission is owner-only; no worker can mutate maps or counters.
            for path in paths {
                if !self.scanned.contains_key(path)
                    && !self.mutable.borrow().observed.contains_key(path)
                {
                    self.admit_path(path, true)?;
                }
            }
            let mut states = Vec::with_capacity(paths.len());
            let mut offset = 0;
            while offset < paths.len() {
                let (count, results) = self.read_batch(
                    &paths[offset..],
                    false,
                    self.limits.max_file_bytes,
                    false,
                    false,
                )?;
                for (path, result) in paths[offset..offset + count].iter().zip(results) {
                    let (actual, _) = result?;
                    let expected = self
                        .scanned
                        .get(path)
                        .map(|file| Observation {
                            expected: ExpectedState::Hash(file.hash.clone()),
                            bytes: file.bytes,
                        })
                        .or_else(|| self.mutable.borrow().observed.get(path).cloned());
                    let state = actual.expected.clone();
                    self.reduce_observation(path, expected, actual)?;
                    states.push(state);
                }
                offset += count;
            }
            Ok(states)
        })
    }
    fn observe_layout(
        &self,
        path: &VaultRelativePath,
        retain: bool,
        ceiling: usize,
    ) -> Result<Option<Vec<u8>>> {
        self.guarded(|| {
            let expected = self.mutable.borrow().layout_observed.get(path).cloned();
            if expected.is_none() {
                self.admit_path(path, true)?;
            }
            let (actual, bytes) = self.read_file(path, retain, ceiling, true)?;
            if let Some(expected) = expected {
                if actual.expected != expected.expected || actual.bytes != expected.bytes {
                    return Err(conflict("maintenance storage layout input changed"));
                }
            } else {
                self.mutable
                    .borrow_mut()
                    .layout_observed
                    .insert(path.clone(), actual);
            }
            Ok(bytes)
        })
    }
    pub(crate) fn final_recheck(&self) -> Result<()> {
        self.guarded(|| {
            let paths = self.census(false)?;
            if paths.len() != self.scanned.len() || !paths.iter().eq(self.scanned.keys()) {
                return Err(conflict("maintenance scanned membership changed"));
            }
            for paths in paths.chunks(maintenance_parallel::MAX_JOBS) {
                self.states_observed(paths)?;
            }
            // Bounded next-key chunks preserve order without a second registry.
            let mut previous: Option<VaultRelativePath> = None;
            loop {
                let paths: Vec<_> = {
                    let state = self.mutable.borrow();
                    let lower = previous
                        .as_ref()
                        .map_or(std::ops::Bound::Unbounded, std::ops::Bound::Excluded);
                    state
                        .observed
                        .range::<VaultRelativePath, _>((lower, std::ops::Bound::Unbounded))
                        .take(maintenance_parallel::MAX_JOBS)
                        .map(|(path, _)| path.clone())
                        .collect()
                };
                let Some(last) = paths.last().cloned() else {
                    break;
                };
                self.states_observed(&paths)?;
                previous = Some(last);
            }
            // These few raw authority files are separate from mapped canonical
            // paths. Rehash the original migration receipt once, not per note.
            let paths: Vec<_> = self
                .mutable
                .borrow()
                .layout_observed
                .keys()
                .cloned()
                .collect();
            for path in paths {
                self.observe_layout(&path, false, 64 * 1024 * 1024)?;
            }
            self.require_clean()
        })
    }
}
impl SourceInputReads for MaintenanceInput {
    fn read_observed(&self, path: &VaultRelativePath, max_bytes: usize) -> Result<Vec<u8>> {
        let bytes = self.guarded(|| {
            if max_bytes == 0 || max_bytes > 64 * 1024 * 1024 {
                return Err(budget("invalid maintenance named-read bound"));
            }
            self.observe(path, true, max_bytes.min(self.limits.max_file_bytes))
        })?;
        bytes.ok_or_else(|| {
            WikiError::new(
                ErrorCode::SourceIntegrity,
                format!("missing payload {path}"),
            )
        })
    }
    fn state_observed(&self, path: &VaultRelativePath) -> Result<ExpectedState> {
        self.guarded(|| {
            self.observe(path, false, self.limits.max_file_bytes)?;
            Ok(self
                .scanned
                .get(path)
                .map(|file| ExpectedState::Hash(file.hash.clone()))
                .unwrap_or_else(|| self.mutable.borrow().observed[path].expected.clone()))
        })
    }
    fn require_clean(&self) -> Result<()> {
        MaintenanceInput::require_clean(self)
    }
}

#[cfg(test)]
#[path = "maintenance_input_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "maintenance_sync_parallel_tests.rs"]
mod parallel_tests;

//! Bounded scanned notes and lazy observed assets for explicit maintenance.
//! Admission counters are logical work/bytes, not a native RSS guarantee.
use super::maintenance_types::{MaintenanceFile, MaintenanceLimits, MaintenanceUsage};
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
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    time::Instant,
};

const RECHECK_WORKERS: usize = 4;

struct OpenedRead {
    file: fs::File,
    absolute: PathBuf,
    metadata: fs::Metadata,
}
struct HashJob {
    file: fs::File,
    ceiling: usize,
    length: u64,
    started: Instant,
    elapsed: std::time::Duration,
    consumed: Arc<AtomicU64>,
    #[cfg(test)]
    path: VaultRelativePath,
    #[cfg(test)]
    hook: Option<ReadHook>,
}
struct HashResult {
    file: fs::File,
    observation: Result<Observation>,
}
struct PendingHash {
    path: VaultRelativePath,
    absolute: PathBuf,
    metadata: fs::Metadata,
    reservation: u64,
    consumed: Arc<AtomicU64>,
}
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReadBoundary {
    BeforeOpen,
    Admitted,
    BeforeChunk,
    AfterChunk,
    Completed,
}
#[cfg(test)]
type ReadHook = Arc<dyn Fn(&VaultRelativePath, ReadBoundary) + Send + Sync>;

// A worker owns only an admitted descriptor and a fixed streaming buffer. The
// owner retains all path checks, mutable accounting, and observation acceptance.
fn hash_admitted(job: &mut HashJob) -> Result<Observation> {
    let mut hash = blake3::Hasher::new();
    let mut length = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        #[cfg(test)]
        if let Some(hook) = &job.hook {
            hook(&job.path, ReadBoundary::BeforeChunk);
        }
        if job.started.elapsed() >= job.elapsed {
            return Err(budget("maintenance input deadline exhausted"));
        }
        // One extra byte detects growth; no worker can consume its neighbours'
        // reservation. Any growth already invalidates the captured length.
        let cap = (job.length + 1 - length).min(buffer.len() as u64) as usize;
        let count = job.file.read(&mut buffer[..cap]).map_err(io)?;
        if count == 0 {
            break;
        }
        job.consumed.fetch_add(count as u64, Ordering::AcqRel);
        length += count as u64;
        #[cfg(test)]
        if let Some(hook) = &job.hook {
            hook(&job.path, ReadBoundary::AfterChunk);
        }
        if length > job.length {
            return Err(if length > job.ceiling as u64 {
                budget("maintenance file grew beyond byte allowance")
            } else {
                conflict("maintenance file length changed while reading")
            });
        }
        hash.update(&buffer[..count]);
    }
    Ok(Observation {
        expected: ExpectedState::Hash(Blake3Hash::new(format!(
            "blake3:{}",
            hash.finalize().to_hex()
        ))?),
        bytes: length,
    })
}
fn hash_worker(jobs: Receiver<HashJob>, results: SyncSender<HashResult>) {
    while let Ok(mut job) = jobs.recv() {
        // Even a failed/panicking stream reports its consumed-byte receipt. A
        // channel failure is also reconciled from that receipt by the owner.
        let observation =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| hash_admitted(&mut job)))
                .unwrap_or_else(|_| {
                    Err(WikiError::new(
                        ErrorCode::Internal,
                        "maintenance hash worker panicked",
                    ))
                });
        #[cfg(test)]
        let observation = if let Some(hook) = &job.hook {
            // Test callbacks also stay inside the worker's panic boundary.
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                hook(&job.path, ReadBoundary::Completed)
            }))
            .is_err()
            {
                Err(WikiError::new(
                    ErrorCode::Internal,
                    "maintenance hash worker panicked",
                ))
            } else {
                observation
            }
        } else {
            observation
        };
        if results
            .send(HashResult {
                file: job.file,
                observation,
            })
            .is_err()
        {
            break;
        }
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
    #[cfg(test)]
    read_hook: Option<ReadHook>,
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
            #[cfg(test)]
            read_hook: None,
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
        for path in paths {
            let remaining = input
                .limits
                .max_retained_note_bytes
                .checked_sub(input.usage().retained_note_bytes)
                .ok_or_else(|| budget("maintenance retained-note counter overflow"))?;
            let retained = canonical_path(&path);
            let ceiling = if retained {
                input.limits.max_file_bytes.min(remaining)
            } else {
                input.limits.max_file_bytes
            };
            if ceiling == 0 {
                return Err(budget("maintenance retained-note allowance exhausted"));
            }
            let (observation, bytes) =
                input.guarded(|| input.read_file(&path, retained, ceiling, false))?;
            if let Some(layout_state) = input.mutable.borrow().layout_observed.get(&path)
                && (layout_state.expected != observation.expected
                    || layout_state.bytes != observation.bytes)
            {
                return Err(conflict(
                    "maintenance vault marker changed after layout validation",
                ));
            }
            let ExpectedState::Hash(hash) = observation.expected else {
                return Err(conflict("scanned canonical path disappeared"));
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
        self.check_clean().map_err(|error| self.latch(error))
    }
    // Parallel admission must not latch a later-path error before earlier
    // admitted reads have been drained and accepted in traversal order.
    fn check_clean(&self) -> Result<()> {
        if let Some(error) = self.mutable.borrow().fatal.clone() {
            return Err(error);
        }
        if self.started.elapsed() >= self.limits.max_elapsed {
            return Err(budget("maintenance input deadline exhausted"));
        }
        Ok(())
    }
    fn guarded<T>(&self, run: impl FnOnce() -> Result<T>) -> Result<T> {
        self.require_clean()?;
        run().map_err(|error| self.latch(error))
    }
    fn path_step(&self) -> Result<()> {
        self.check_clean()?;
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
    fn remaining_io(&self) -> u64 {
        self.limits
            .max_io_bytes
            .saturating_sub(self.usage().io_bytes)
    }
    fn charge_io(&self, bytes: usize, layout: bool) -> Result<()> {
        let mut state = self.mutable.borrow_mut();
        state.usage.io_bytes = state
            .usage
            .io_bytes
            .checked_add(bytes as u64)
            .filter(|n| *n <= self.limits.max_io_bytes)
            .ok_or_else(|| budget("maintenance IO allowance exhausted"))?;
        if layout {
            state.usage.layout_io_bytes += bytes as u64;
        }
        Ok(())
    }
    fn open_read(
        &self,
        path: &VaultRelativePath,
        ceiling: usize,
        raw: bool,
    ) -> Result<Option<OpenedRead>> {
        self.check_clean()?;
        let physical = if raw {
            path.clone()
        } else {
            self.layout
                .as_ref()
                .expect("validated maintenance layout")
                .map(self.fs.root(), path)?
        };
        let absolute = self
            .fs
            .root()
            .resolve_raw_budgeted(&physical, &mut || self.path_step())?;
        let before = match fs::symlink_metadata(&absolute) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) => return Err(io(error)),
        };
        if !before.is_file() {
            return Err(conflict("maintenance input is not a regular file"));
        }
        if before.len() > ceiling as u64 || before.len() > self.remaining_io() {
            return Err(budget(
                "maintenance input exceeds byte allowance before allocation",
            ));
        }
        #[cfg(test)]
        if let Some(hook) = &self.read_hook {
            hook(path, ReadBoundary::BeforeOpen);
        }
        #[cfg(unix)]
        let file = {
            use std::os::unix::fs::OpenOptionsExt;
            fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&absolute)
                .map_err(io)?
        };
        #[cfg(not(unix))]
        let file = fs::File::open(&absolute).map_err(io)?;
        let opened = file.metadata().map_err(io)?;
        Self::same_file(&before, &opened)?;
        if opened.len() > ceiling as u64 || opened.len() > self.remaining_io() {
            return Err(budget(
                "maintenance opened input exceeds byte allowance before allocation",
            ));
        }
        #[cfg(test)]
        if let Some(hook) = &self.read_hook {
            hook(path, ReadBoundary::Admitted);
        }
        Ok(Some(OpenedRead {
            file,
            absolute,
            metadata: opened,
        }))
    }
    fn read_file(
        &self,
        path: &VaultRelativePath,
        retain: bool,
        ceiling: usize,
        raw: bool,
    ) -> Result<(Observation, Option<Vec<u8>>)> {
        let Some(opened) = self.open_read(path, ceiling, raw)? else {
            return Ok((
                Observation {
                    expected: ExpectedState::Absent,
                    bytes: 0,
                },
                None,
            ));
        };
        self.read_opened(opened, retain, ceiling, raw)
    }
    fn read_opened(
        &self,
        input: OpenedRead,
        retain: bool,
        ceiling: usize,
        raw: bool,
    ) -> Result<(Observation, Option<Vec<u8>>)> {
        let OpenedRead {
            mut file,
            absolute,
            metadata: opened,
        } = input;
        let mut output = retain.then(Vec::new);
        if let Some(bytes) = &mut output {
            bytes
                .try_reserve_exact(
                    usize::try_from(opened.len())
                        .map_err(|_| budget("maintenance file length conversion"))?,
                )
                .map_err(|_| budget("maintenance payload allocation failed"))?;
        }
        let mut hash = blake3::Hasher::new();
        let mut length = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            self.require_clean()?;
            let cap = (ceiling as u64 + 1 - length)
                .min(self.remaining_io())
                .min(buffer.len() as u64) as usize;
            if cap == 0 {
                if file.metadata().map_err(io)?.len() == length {
                    break;
                }
                return Err(budget("maintenance IO allowance exhausted while reading"));
            }
            let count = file.read(&mut buffer[..cap]).map_err(io)?;
            if count == 0 {
                break;
            }
            self.charge_io(count, raw)?;
            length = length
                .checked_add(count as u64)
                .ok_or_else(|| budget("maintenance file byte counter overflow"))?;
            if length > ceiling as u64 {
                return Err(budget("maintenance file grew beyond byte allowance"));
            }
            hash.update(&buffer[..count]);
            if let Some(bytes) = &mut output {
                bytes
                    .try_reserve_exact(count)
                    .map_err(|_| budget("maintenance payload growth allocation failed"))?;
                bytes.extend_from_slice(&buffer[..count]);
            }
        }
        let after = file.metadata().map_err(io)?;
        Self::same_file(&opened, &after)?;
        let named = fs::symlink_metadata(&absolute).map_err(io)?;
        Self::same_file(&opened, &named)?;
        if after.len() != length || opened.len() != length {
            return Err(conflict("maintenance file length changed while reading"));
        }
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
        Ok(bytes)
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
    fn next_recheck_path(
        &self,
        canonical: &mut impl Iterator<Item = VaultRelativePath>,
        previous: &mut Option<VaultRelativePath>,
    ) -> Option<VaultRelativePath> {
        if let Some(path) = canonical.next() {
            return Some(path);
        }
        // Keep one auxiliary key, never another complete manifest.
        let state = self.mutable.borrow();
        let next = match previous.as_ref() {
            Some(path) => state
                .observed
                .range::<VaultRelativePath, _>((
                    std::ops::Bound::Excluded(path),
                    std::ops::Bound::Unbounded,
                ))
                .next(),
            None => state.observed.iter().next(),
        }
        .map(|(path, _)| path.clone());
        *previous = next.clone().or_else(|| previous.clone());
        next
    }
    fn expected_recheck(&self, path: &VaultRelativePath) -> Observation {
        self.scanned
            .get(path)
            .map(|file| Observation {
                expected: ExpectedState::Hash(file.hash.clone()),
                bytes: file.bytes,
            })
            .unwrap_or_else(|| self.mutable.borrow().observed[path].clone())
    }
    fn accept_recheck(&self, path: &VaultRelativePath, actual: Observation) -> Result<()> {
        let expected = self.expected_recheck(path);
        if actual.expected != expected.expected || actual.bytes != expected.bytes {
            return Err(conflict(&format!(
                "maintenance observed input changed: {path}"
            )));
        }
        Ok(())
    }
    fn drain_hashes(
        &self,
        pending: &mut Vec<PendingHash>,
        workers: &[(SyncSender<HashJob>, Receiver<HashResult>)],
        reserved: &mut u64,
    ) -> Result<()> {
        let mut first = None;
        for (slot, pending) in pending.drain(..).enumerate() {
            let result = workers[slot].1.recv();
            let consumed = pending.consumed.load(Ordering::Acquire);
            *reserved -= pending.reservation;
            // Charge every successful or failed read exactly once, even when an
            // earlier result already failed. Reservations are never actual IO.
            let charged = self.charge_io(consumed as usize, false);
            let accepted = charged.and_then(|()| {
                let result = result.map_err(|_| {
                    WikiError::new(ErrorCode::Internal, "maintenance hash worker disconnected")
                })?;
                let observation = result.observation?;
                let after = result.file.metadata().map_err(io)?;
                Self::same_file(&pending.metadata, &after)?;
                let named = fs::symlink_metadata(&pending.absolute).map_err(io)?;
                Self::same_file(&pending.metadata, &named)?;
                if after.len() != observation.bytes || pending.metadata.len() != observation.bytes {
                    return Err(conflict("maintenance file length changed while reading"));
                }
                self.accept_recheck(&pending.path, observation)?;
                self.check_clean()
            });
            if let Err(mut error) = accepted {
                if first.is_none() {
                    error.message = format!("{} [{}]", error.message, pending.path);
                    first = Some(error);
                }
            }
        }
        first.map_or(Ok(()), Err)
    }
    fn recheck_hashes(&self, paths: Vec<VaultRelativePath>, parallel: bool) -> Result<()> {
        std::thread::scope(|scope| {
            let mut workers = Vec::new();
            let mut handles = Vec::new();
            if parallel {
                for slot in 0..RECHECK_WORKERS {
                    let (jobs, receiver) = mpsc::sync_channel(1);
                    let (sender, results) = mpsc::sync_channel(1);
                    match std::thread::Builder::new()
                        .name(format!("maintenance-hash-{slot}"))
                        .spawn_scoped(scope, move || hash_worker(receiver, sender))
                    {
                        Ok(handle) => {
                            handles.push(handle);
                            workers.push((jobs, results));
                        }
                        // No jobs have been admitted yet. A partial pool is safe;
                        // if none could start, preserve the internal serial path.
                        Err(_) => break,
                    }
                }
            }
            let mut serial = workers.is_empty();
            let mut pending = Vec::with_capacity(RECHECK_WORKERS);
            let mut reserved = 0u64;
            let mut canonical = paths.into_iter();
            let mut previous = None;
            let run = (|| {
                while let Some(path) = self.next_recheck_path(&mut canonical, &mut previous) {
                    if serial {
                        self.observe(&path, false, self.limits.max_file_bytes)?;
                        continue;
                    }
                    let Some(opened) = self.open_read(&path, self.limits.max_file_bytes, false)?
                    else {
                        self.accept_recheck(
                            &path,
                            Observation {
                                expected: ExpectedState::Absent,
                                bytes: 0,
                            },
                        )?;
                        continue;
                    };
                    let reservation = opened.metadata.len() + 1;
                    if reservation > self.remaining_io().saturating_sub(reserved) {
                        // Conservative growth-byte reservations must not reject
                        // an input admissible by the original serial reader.
                        self.drain_hashes(&mut pending, &workers, &mut reserved)?;
                        let (actual, _) =
                            self.read_opened(opened, false, self.limits.max_file_bytes, false)?;
                        self.accept_recheck(&path, actual)?;
                        serial = true;
                        continue;
                    }
                    let consumed = Arc::new(AtomicU64::new(0));
                    let slot = pending.len();
                    let job = HashJob {
                        file: opened.file,
                        ceiling: self.limits.max_file_bytes,
                        length: opened.metadata.len(),
                        started: self.started,
                        elapsed: self.limits.max_elapsed,
                        consumed: consumed.clone(),
                        #[cfg(test)]
                        path: path.clone(),
                        #[cfg(test)]
                        hook: self.read_hook.clone(),
                    };
                    // Record the reservation before the descriptor can reach a
                    // worker. A failed send returns an unread job to the owner.
                    reserved += reservation;
                    if workers[slot].0.send(job).is_err() {
                        reserved -= reservation;
                        return Err(WikiError::new(
                            ErrorCode::Internal,
                            "maintenance hash worker disconnected before dispatch",
                        ));
                    }
                    pending.push(PendingHash {
                        path,
                        absolute: opened.absolute,
                        metadata: opened.metadata,
                        reservation,
                        consumed,
                    });
                    if pending.len() == workers.len() {
                        self.drain_hashes(&mut pending, &workers, &mut reserved)?;
                    }
                }
                Ok(())
            })();
            // Earlier admitted paths take precedence over a later admission
            // failure. Drain before latching, and never detach an active reader.
            let drained = self.drain_hashes(&mut pending, &workers, &mut reserved);
            drop(workers);
            let mut joined = Ok(());
            for handle in handles {
                if handle.join().is_err() {
                    joined = Err(WikiError::new(
                        ErrorCode::Internal,
                        "maintenance hash worker panicked",
                    ));
                }
            }
            drained.and(run).and(joined)
        })
    }
    pub(crate) fn final_recheck(&self) -> Result<()> {
        self.final_recheck_mode(true)
    }
    // Retained internally for exact stable-input comparisons and safe fallback.
    fn final_recheck_mode(&self, parallel: bool) -> Result<()> {
        self.guarded(|| {
            let paths = self.census(false)?;
            if paths.len() != self.scanned.len() || !paths.iter().eq(self.scanned.keys()) {
                return Err(conflict("maintenance scanned membership changed"));
            }
            self.recheck_hashes(paths, parallel)?;
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

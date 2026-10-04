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
use std::{cell::RefCell, collections::BTreeMap, fs, io::Read, time::Instant};

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
    fn read_file(
        &self,
        path: &VaultRelativePath,
        retain: bool,
        ceiling: usize,
        raw: bool,
    ) -> Result<(Observation, Option<Vec<u8>>)> {
        self.require_clean()?;
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
        if before.len() > ceiling as u64 || before.len() > self.remaining_io() {
            return Err(budget(
                "maintenance input exceeds byte allowance before allocation",
            ));
        }
        #[cfg(unix)]
        let mut file = {
            use std::os::unix::fs::OpenOptionsExt;
            fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&absolute)
                .map_err(io)?
        };
        #[cfg(not(unix))]
        let mut file = fs::File::open(&absolute).map_err(io)?;
        let opened = file.metadata().map_err(io)?;
        Self::same_file(&before, &opened)?;
        if opened.len() > ceiling as u64 || opened.len() > self.remaining_io() {
            return Err(budget(
                "maintenance opened input exceeds byte allowance before allocation",
            ));
        }
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
    pub(crate) fn final_recheck(&self) -> Result<()> {
        self.guarded(|| {
            let paths = self.census(false)?;
            if paths.len() != self.scanned.len() || !paths.iter().eq(self.scanned.keys()) {
                return Err(conflict("maintenance scanned membership changed"));
            }
            for path in paths {
                self.observe(&path, false, self.limits.max_file_bytes)?;
            }
            // Keep only one next key; don't allocate a second auxiliary manifest.
            let mut previous: Option<VaultRelativePath> = None;
            loop {
                let next = {
                    let state = self.mutable.borrow();
                    match previous.as_ref() {
                        Some(path) => state
                            .observed
                            .range::<VaultRelativePath, _>((
                                std::ops::Bound::Excluded(path),
                                std::ops::Bound::Unbounded,
                            ))
                            .next(),
                        None => state.observed.iter().next(),
                    }
                    .map(|(path, _)| path.clone())
                };
                let Some(path) = next else {
                    break;
                };
                self.observe(&path, false, self.limits.max_file_bytes)?;
                previous = Some(path);
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

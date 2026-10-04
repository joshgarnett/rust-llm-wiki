//! Finite operational import storage. These records are comparison/progress
//! evidence; they never grant canonical write or automatic-apply authority.
use super::source_import_types::*;
use crate::{
    catalog::source_refresh::NamedIndexedIntent,
    changes::{ChangeEngine, journal::require_sync, prepare::strict_json},
    domain::{Blake3Hash, ErrorCode, RecordId, Result, VaultRelativePath, WikiError},
    sources::import_manifest_types::{
        MAX_IMPORT_ITEM_BYTES, MAX_IMPORT_ITEMS, MAX_IMPORT_MANIFEST_BYTES, ManifestPreparation,
    },
    vault::{DirectorySync, DurableIo, ExpectedState, VaultFs, WriterPermit},
};
use std::{
    fs::{self, File, Metadata},
    io::{Read, Seek, SeekFrom},
    path::{Component, Path},
    sync::Arc,
};

const ANCHOR: &str = ".wiki/state/source-imports";
const INTENTS: &str = ".wiki/state/source-imports/intents";
fn conflict(message: impl Into<String>) -> WikiError {
    WikiError::new(ErrorCode::ContentConflict, message)
}
fn budget(message: &str) -> WikiError {
    WikiError::new(ErrorCode::BudgetExceeded, message)
}
fn io_error(error: impl std::fmt::Display) -> WikiError {
    WikiError::new(
        ErrorCode::Internal,
        format!("import operational file: {error}"),
    )
}
fn encoded<T: serde::Serialize>(value: &T, max: usize) -> Result<Vec<u8>> {
    struct BoundedBytes {
        bytes: Vec<u8>,
        max: usize,
        overflow: bool,
    }
    impl std::io::Write for BoundedBytes {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.max.saturating_sub(self.bytes.len()) {
                self.overflow = true;
                return Err(std::io::Error::other("import encoding bound"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = BoundedBytes {
        bytes: Vec::new(),
        max,
        overflow: false,
    };
    if let Err(error) = serde_json::to_writer(&mut output, value) {
        return Err(if output.overflow {
            budget("import operational encoding exceeds bound")
        } else {
            io_error(error)
        });
    }
    Ok(output.bytes)
}

/// Only creation protection changes. Every intercepted mutation delegates once
/// to the original adapter, including profiling/fault adapters. The outer Arc
/// is a proxy; it must not be reported as identical to its retained inner Arc.
struct PrivateCreationIo {
    inner: Arc<dyn DurableIo>,
}
impl DurableIo for PrivateCreationIo {
    fn create_stage(&self, p: &Path) -> std::io::Result<File> {
        self.inner.create_private_stage(p)
    }
    fn create_private_stage(&self, p: &Path) -> std::io::Result<File> {
        self.inner.create_private_stage(p)
    }
    fn create_private_directory(&self, p: &Path) -> std::io::Result<()> {
        self.inner.create_private_directory(p)
    }
    fn create_directory(&self, p: &Path) -> std::io::Result<()> {
        self.inner.create_private_directory(p)
    }
    fn open_append(&self, p: &Path) -> std::io::Result<File> {
        self.inner.open_append(p)
    }
    fn truncate_file(&self, f: &File, n: u64) -> std::io::Result<()> {
        self.inner.truncate_file(f, n)
    }
    fn write_stage(&self, f: &mut File, b: &[u8]) -> std::io::Result<()> {
        self.inner.write_stage(f, b)
    }
    fn sync_file(&self, f: &File) -> std::io::Result<()> {
        self.inner.sync_file(f)
    }
    fn replace(&self, a: &Path, b: &Path) -> std::io::Result<()> {
        self.inner.replace(a, b)
    }
    fn remove(&self, p: &Path) -> std::io::Result<()> {
        self.inner.remove(p)
    }
    fn remove_directory(&self, p: &Path) -> std::io::Result<()> {
        self.inner.remove_directory(p)
    }
    fn sync_directory(&self, p: &Path) -> std::io::Result<DirectorySync> {
        self.inner.sync_directory(p)
    }
}

pub(crate) struct ImportStore {
    fs: VaultFs,
    vault_id: RecordId,
    key: String,
    state: VaultRelativePath,
    manifest: VaultRelativePath,
    results: VaultRelativePath,
}
impl ImportStore {
    /// Pure: no marker reads, directory inspection or allocation on disk.
    pub(crate) fn new(fs: VaultFs, vault_id: RecordId, key: &str) -> Result<Self> {
        if key.is_empty() || key.len() > 512 || key.chars().any(char::is_control) {
            return Err(WikiError::invalid(
                "import key requires 1–512 UTF-8 bytes without controls",
            ));
        }
        let prefix = format!("{ANCHOR}/{}", Blake3Hash::digest(key.as_bytes()).hex());
        let protected = VaultFs::with_io(
            fs.root().clone(),
            Arc::new(PrivateCreationIo {
                inner: fs.durable_io(),
            }),
        );
        Ok(Self {
            fs: protected,
            vault_id,
            key: key.into(),
            state: VaultRelativePath::new(format!("{prefix}.json"))?,
            manifest: VaultRelativePath::new(format!("{prefix}.manifest.jsonl"))?,
            results: VaultRelativePath::new(format!("{prefix}.results.jsonl"))?,
        })
    }
    pub(crate) fn manifest_path(&self) -> VaultRelativePath {
        self.manifest.clone()
    }
    pub(crate) fn results_path(&self) -> VaultRelativePath {
        self.results.clone()
    }
    fn require_qualified(&self) -> Result<()> {
        #[cfg(not(unix))]
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "single-link import operational storage is not qualified on this platform",
        ));
        #[cfg(unix)]
        Ok(())
    }
    fn checked(&self, path: &VaultRelativePath) -> Result<()> {
        self.require_qualified()?;
        self.fs.validate_paths(std::slice::from_ref(path))?;
        let engine = ChangeEngine::new(self.fs.clone())?;
        if engine.vault_id != self.vault_id {
            return Err(conflict("import store belongs to another vault"));
        }
        engine.require_named_single_link(path)
    }
    fn open(&self, path: &VaultRelativePath, max: u64) -> Result<Option<(File, Metadata)>> {
        self.checked(path)?;
        let actual = self.fs.root().resolve(path)?;
        let before = match fs::symlink_metadata(&actual) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_error(error)),
        };
        if !before.is_file() || before.len() > max {
            return Err(budget("import file type or size is outside its bound"));
        }
        let file = File::open(&actual).map_err(io_error)?;
        let opened = file.metadata().map_err(io_error)?;
        let after = fs::symlink_metadata(actual).map_err(io_error)?;
        if !same_file(&before, &opened) || !same_file(&opened, &after) {
            return Err(conflict("import file identity changed during open"));
        }
        Ok(Some((file, opened)))
    }
    fn finish_read(&self, path: &VaultRelativePath, file: &File, before: &Metadata) -> Result<()> {
        let opened = file.metadata().map_err(io_error)?;
        let named = fs::symlink_metadata(self.fs.root().resolve(path)?).map_err(io_error)?;
        if !same_file(before, &opened)
            || !same_file(&opened, &named)
            || opened.len() != before.len()
            || opened.modified().ok() != before.modified().ok()
        {
            return Err(conflict("import file changed while reading"));
        }
        self.checked(path)
    }
    fn bytes(&self, path: &VaultRelativePath, max: usize) -> Result<Option<Vec<u8>>> {
        let Some((mut file, before)) = self.open(path, max as u64)? else {
            return Ok(None);
        };
        let mut bytes = Vec::new();
        (&mut file)
            .take(max as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(io_error)?;
        if bytes.len() > max || bytes.len() as u64 != before.len() {
            return Err(budget("import bounded read changed length"));
        }
        self.finish_read(path, &file, &before)?;
        Ok(Some(bytes))
    }
    pub(crate) fn initialize_directory(&self, writer: &WriterPermit) -> Result<()> {
        self.require_qualified()?;
        writer.require_root(self.fs.root())?;
        // Shared fixed anchors only: no unacknowledged per-key mkdir ownership.
        for anchor in [ANCHOR, INTENTS] {
            let path = VaultRelativePath::new(anchor)?;
            self.fs.validate_paths(std::slice::from_ref(&path))?;
            let engine = ChangeEngine::new(self.fs.clone())?;
            if engine.vault_id != self.vault_id {
                return Err(conflict("import store vault changed"));
            }
            require_sync(self.fs.ensure_directory(&path, writer)?)?;
        }
        Ok(())
    }
    pub(crate) fn initial_results_hash(
        &self,
        manifest: &StoredImportManifest,
    ) -> Result<Blake3Hash> {
        validate_manifest_summary(manifest)?;
        Ok(Blake3Hash::digest(encoded(
            &(IMPORT_STATE_VERSION, &self.vault_id, &self.key, manifest),
            MAX_IMPORT_STATE_BYTES,
        )?))
    }
    fn validate_progress(&self, p: &ImportProgress) -> Result<()> {
        validate_manifest_summary(&p.manifest)?;
        if p.version != IMPORT_STATE_VERSION
            || p.vault_id != self.vault_id
            || p.key != self.key
            || !(1..=8).contains(&p.group_size)
            || p.next_ordinal > p.manifest.items
            || p.manifest_offset < p.manifest.first_item_offset
            || p.manifest_offset > p.manifest.bytes
            || p.groups_committed > p.next_ordinal
            || p.results_offset > MAX_IMPORT_RESULTS_BYTES
            || (p.completed && (p.next_ordinal != p.manifest.items || p.pending.is_some()))
            || (p.results_offset == 0
                && p.results_hash != self.initial_results_hash(&p.manifest)?)
            || (p.next_ordinal == 0
                && (p.groups_committed != 0
                    || p.last_group.is_some()
                    || p.manifest_offset != p.manifest.first_item_offset))
        {
            return Err(conflict("import progress header, cursor or limits differ"));
        }
        match &p.last_group {
            Some(last) => {
                validate_result(last)?;
                if last.group.checked_add(1) != Some(p.groups_committed)
                    || last
                        .items
                        .last()
                        .and_then(|item| item.ordinal.checked_add(1))
                        != Some(p.next_ordinal)
                {
                    return Err(conflict("import last group differs from committed cursor"));
                }
            }
            None if p.groups_committed != 0 || p.next_ordinal != 0 => {
                return Err(conflict("import committed cursor lacks its last group"));
            }
            None => {}
        }
        if let Some(pending) = &p.pending {
            validate_change(&pending.change)?;
            if pending.group != p.groups_committed
                || pending.first_ordinal != p.next_ordinal
                || pending.captures.is_empty()
                || pending.captures.len() > p.group_size
                || pending.next_manifest_offset <= p.manifest_offset
                || pending.next_manifest_offset > p.manifest.bytes
                || p.next_ordinal
                    .checked_add(pending.captures.len() as u64)
                    .is_none_or(|end| end > p.manifest.items)
            {
                return Err(conflict("import pending group differs from bounded cursor"));
            }
            let mut ids = std::collections::BTreeSet::new();
            for (index, capture) in pending.captures.iter().enumerate() {
                let item = &capture.item;
                encoded(
                    item,
                    crate::sources::import_manifest_types::MAX_IMPORT_LINE_BYTES,
                )?;
                let path = Path::new(&item.path);
                if item.ordinal != p.next_ordinal + index as u64
                    || item.byte_len > MAX_IMPORT_ITEM_BYTES
                    || !path.is_absolute()
                    || path
                        .components()
                        .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
                    || path
                        .components()
                        .collect::<std::path::PathBuf>()
                        .as_os_str()
                        != path.as_os_str()
                    || item.path.contains('\0')
                    || capture.allocation.source_id == pending.change.change_id
                    || capture.allocation.revision_id == pending.change.change_id
                    || item.title.trim().is_empty()
                    || !ids.insert(&capture.allocation.source_id)
                    || !ids.insert(&capture.allocation.revision_id)
                {
                    return Err(conflict(
                        "import pending capture identities or item metadata differ",
                    ));
                }
                timestamp(&capture.allocation.captured_at)?;
            }
            if let Some(intent) = &pending.intent {
                self.intent_path(intent)?;
            }
            if let Some(intent) = &pending.input_intent {
                self.intent_path(intent)?;
            }
        }
        Ok(())
    }
    pub(crate) fn load(&self) -> Result<Option<(ImportProgress, Blake3Hash)>> {
        let Some(raw) = self.bytes(&self.state, MAX_IMPORT_STATE_BYTES)? else {
            return Ok(None);
        };
        let envelope: ImportStateEnvelope = strict_json(&raw)?;
        self.validate_progress(&envelope.progress)?;
        if envelope.checksum
            != Blake3Hash::digest(encoded(&envelope.progress, MAX_IMPORT_STATE_BYTES)?)
        {
            return Err(conflict("import state checksum differs"));
        }
        match self.open(&self.results, MAX_IMPORT_RESULTS_BYTES)? {
            Some((_, metadata)) if metadata.len() >= envelope.progress.results_offset => {}
            None if envelope.progress.results_offset == 0 => {}
            _ => {
                return Err(conflict(
                    "acknowledged import result mappings are missing or truncated",
                ));
            }
        }
        Ok(Some((envelope.progress, Blake3Hash::digest(raw))))
    }
    pub(crate) fn save(
        &self,
        writer: &WriterPermit,
        progress: &ImportProgress,
        expected: ExpectedState,
    ) -> Result<Blake3Hash> {
        writer.require_root(self.fs.root())?;
        self.validate_progress(progress)?;
        let previous = self.load()?;
        match (&expected, &previous) {
            (ExpectedState::Absent, None) => {
                // A new header cannot adopt a foreign future flat payload.
                for path in [&self.manifest, &self.results] {
                    self.checked(path)?;
                    match fs::symlink_metadata(self.fs.root().resolve(path)?) {
                        Ok(_) => {
                            return Err(conflict(
                                "unowned import payload precedes its initial header",
                            ));
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(io_error(error)),
                    }
                }
            }
            (ExpectedState::Hash(hash), Some((old, actual)))
                if hash == actual
                    && old.manifest == progress.manifest
                    && old.group_size == progress.group_size => {}
            _ => {
                return Err(conflict(
                    "import state expected header or immutable manifest differs",
                ));
            }
        }
        let envelope = ImportStateEnvelope {
            progress: progress.clone(),
            checksum: Blake3Hash::digest(encoded(progress, MAX_IMPORT_STATE_BYTES)?),
        };
        let bytes = encoded(&envelope, MAX_IMPORT_STATE_BYTES)?;
        self.checked(&self.state)?;
        let stage = self.fs.stage(&self.state, &bytes, writer)?;
        self.checked(&self.state)?;
        require_sync(self.fs.replace(stage, &expected, writer)?)?;
        Ok(Blake3Hash::digest(bytes))
    }
    pub(crate) fn install_manifest(
        &self,
        writer: &WriterPermit,
        source: &Path,
        summary: &ManifestPreparation,
    ) -> Result<()> {
        writer.require_root(self.fs.root())?;
        let (progress, _) = self
            .load()?
            .ok_or_else(|| conflict("manifest requires its acknowledged progress header"))?;
        if summary.preview
            || summary.manifest_hash != progress.manifest.hash
            || summary.items != progress.manifest.items
            || summary.manifest_bytes != progress.manifest.bytes
            || summary.first_item_offset != progress.manifest.first_item_offset
        {
            return Err(conflict("manifest summary differs from frozen import"));
        }
        // A fully installed, known manifest survives a lost copy acknowledgement
        // even after the external file disappears. Header identity and exact
        // retained bytes, not mere path presence, authorize this resync.
        if let Some(existing) = self.bytes(&self.manifest, MAX_IMPORT_MANIFEST_BYTES as usize)? {
            if existing.len() as u64 != summary.manifest_bytes
                || Blake3Hash::digest(&existing) != summary.manifest_hash
            {
                return Err(conflict("owned manifest target differs from frozen bytes"));
            }
            self.checked(&self.manifest)?;
            return require_sync(self.fs.sync_target(&self.manifest, writer)?);
        }
        let before = fs::symlink_metadata(source).map_err(io_error)?;
        if !before.is_file()
            || before.file_type().is_symlink()
            || before.len() != summary.manifest_bytes
            || before.len() > MAX_IMPORT_MANIFEST_BYTES
        {
            return Err(conflict("manifest source type or length differs"));
        }
        let mut file = File::open(source).map_err(io_error)?;
        let opened = file.metadata().map_err(io_error)?;
        if !same_file(&before, &opened) {
            return Err(conflict("manifest source identity changed"));
        }
        let mut bytes = Vec::new();
        (&mut file)
            .take(MAX_IMPORT_MANIFEST_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(io_error)?;
        let after = file.metadata().map_err(io_error)?;
        let named = fs::symlink_metadata(source).map_err(io_error)?;
        if bytes.len() as u64 != summary.manifest_bytes
            || Blake3Hash::digest(&bytes) != summary.manifest_hash
            || !same_file(&opened, &named)
            || after.len() != opened.len()
            || after.modified().ok() != opened.modified().ok()
        {
            return Err(conflict("manifest source bytes changed"));
        }
        self.install(
            writer,
            &self.manifest,
            &bytes,
            MAX_IMPORT_MANIFEST_BYTES as usize,
        )
    }
    fn install(
        &self,
        writer: &WriterPermit,
        path: &VaultRelativePath,
        bytes: &[u8],
        max: usize,
    ) -> Result<()> {
        if bytes.len() > max {
            return Err(budget("import immutable file exceeds bound"));
        }
        match self.bytes(path, max)? {
            Some(old) if old == bytes => {
                self.checked(path)?;
                require_sync(self.fs.sync_target(path, writer)?)
            }
            Some(_) => Err(conflict(
                "import immutable target contains unfamiliar bytes",
            )),
            None => {
                self.checked(path)?;
                let stage = self.fs.stage(path, bytes, writer)?;
                self.checked(path)?;
                require_sync(self.fs.replace(stage, &ExpectedState::Absent, writer)?)
            }
        }
    }
    fn validate_intent(&self, intent: &NamedIndexedIntent) -> Result<()> {
        if intent.manifest.vault_id != self.vault_id
            || intent.proof.vault_id != self.vault_id
            || intent.proof.version != 3
            || intent.proof.source_id.is_some()
            || intent
                .proof
                .operation
                .as_ref()
                .is_none_or(|operation| operation.capture_targets().is_empty())
        {
            return Err(conflict(
                "import intent vault or capture descriptor differs",
            ));
        }
        intent.proof.validate_manifest(&intent.manifest)?;
        if intent.proof.change.manifest_hash
            != Blake3Hash::digest(encoded(&intent.manifest, MAX_IMPORT_INTENT_BYTES)?)
        {
            return Err(conflict("import intent manifest hash differs"));
        }
        Ok(())
    }
    fn intent_path(&self, reference: &ImportIntentRef) -> Result<()> {
        if reference.path.as_str() != format!("{INTENTS}/{}.json", reference.hash.hex()) {
            return Err(conflict(
                "import intent reference is outside its exact content address",
            ));
        }
        Ok(())
    }
    pub(crate) fn retain_intent(
        &self,
        writer: &WriterPermit,
        intent: &NamedIndexedIntent,
    ) -> Result<ImportIntentRef> {
        writer.require_root(self.fs.root())?;
        self.load()?
            .ok_or_else(|| conflict("intent requires its acknowledged import header"))?;
        self.validate_intent(intent)?;
        let bytes = encoded(intent, MAX_IMPORT_INTENT_BYTES)?;
        let hash = Blake3Hash::digest(&bytes);
        let reference = ImportIntentRef {
            path: VaultRelativePath::new(format!("{INTENTS}/{}.json", hash.hex()))?,
            hash,
        };
        self.install(writer, &reference.path, &bytes, MAX_IMPORT_INTENT_BYTES)?;
        Ok(reference)
    }
    pub(crate) fn read_intent(&self, reference: &ImportIntentRef) -> Result<NamedIndexedIntent> {
        self.intent_path(reference)?;
        let bytes = self
            .bytes(&reference.path, MAX_IMPORT_INTENT_BYTES)?
            .ok_or_else(|| conflict("known import intent is absent"))?;
        if Blake3Hash::digest(&bytes) != reference.hash {
            return Err(conflict("known import intent hash differs"));
        }
        let intent = strict_json(&bytes)?;
        self.validate_intent(&intent)?;
        Ok(intent)
    }
    pub(crate) fn append_result(
        &self,
        writer: &WriterPermit,
        expected_offset: u64,
        previous: &Blake3Hash,
        event: ImportResultEvent,
    ) -> Result<(u64, Blake3Hash)> {
        writer.require_root(self.fs.root())?;
        let (progress, _) = self
            .load()?
            .ok_or_else(|| conflict("result append requires known progress"))?;
        if progress.results_offset != expected_offset || &progress.results_hash != previous {
            return Err(conflict("result append differs from acknowledged cursor"));
        }
        validate_event(&event)?;
        event_matches_pending(&event, &progress)?;
        let checksum =
            Blake3Hash::digest(encoded(&(previous, &event), MAX_IMPORT_RESULT_LINE_BYTES)?);
        let frame = ImportResultFrame {
            previous: previous.clone(),
            event,
            checksum: checksum.clone(),
        };
        let mut bytes = encoded(&frame, MAX_IMPORT_RESULT_LINE_BYTES - 1)?;
        bytes.push(b'\n');
        let next = expected_offset
            .checked_add(bytes.len() as u64)
            .filter(|length| *length <= MAX_IMPORT_RESULTS_BYTES)
            .ok_or_else(|| budget("import result journal exceeds 64 MiB"))?;
        let existing = self.open(&self.results, MAX_IMPORT_RESULTS_BYTES)?;
        match existing {
            Some((_, metadata)) if metadata.len() == expected_offset => {}
            None if expected_offset == 0 => self.install(writer, &self.results, b"", 0)?,
            _ => {
                return Err(conflict(
                    "import result append length changed or tail is unacknowledged",
                ));
            }
        }
        self.checked(&self.results)?;
        require_sync(self.fs.append_synced(&self.results, &bytes, writer)?)?;
        let (_, metadata) = self
            .open(&self.results, MAX_IMPORT_RESULTS_BYTES)?
            .ok_or_else(|| conflict("result append disappeared"))?;
        if metadata.len() != next {
            return Err(conflict("result append length differs after durability"));
        }
        Ok((next, checksum))
    }
    pub(crate) fn read_tail(&self, offset: u64, previous: &Blake3Hash) -> Result<ImportResultTail> {
        let (progress, _) = self
            .load()?
            .ok_or_else(|| conflict("result tail requires known progress"))?;
        if progress.results_offset != offset || &progress.results_hash != previous {
            return Err(conflict("result tail differs from acknowledged cursor"));
        }
        let Some((mut file, before)) = self.open(&self.results, MAX_IMPORT_RESULTS_BYTES)? else {
            if offset != 0 {
                return Err(conflict("acknowledged result journal disappeared"));
            }
            return Ok(ImportResultTail {
                frame: None,
                next_offset: offset,
                torn_tail: false,
            });
        };
        if offset > before.len() || before.len() - offset > 2 * MAX_IMPORT_RESULT_LINE_BYTES as u64
        {
            return Err(budget(
                "import result tail exceeds one frame plus bounded fragment",
            ));
        }
        file.seek(SeekFrom::Start(offset)).map_err(io_error)?;
        let mut bytes = Vec::new();
        (&mut file)
            .take(2 * MAX_IMPORT_RESULT_LINE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(io_error)?;
        if bytes.len() as u64 != before.len() - offset {
            return Err(conflict("import tail length changed while reading"));
        }
        self.finish_read(&self.results, &file, &before)?;
        match bytes.iter().position(|byte| *byte == b'\n') {
            None => {
                if bytes.len() > MAX_IMPORT_RESULT_LINE_BYTES {
                    return Err(budget("import torn result fragment exceeds bound"));
                }
                Ok(ImportResultTail {
                    frame: None,
                    next_offset: offset,
                    torn_tail: !bytes.is_empty(),
                })
            }
            Some(end) => {
                if end + 1 > MAX_IMPORT_RESULT_LINE_BYTES
                    || bytes[end + 1..].contains(&b'\n')
                    || bytes.len() - end - 1 > MAX_IMPORT_RESULT_LINE_BYTES
                {
                    return Err(conflict(
                        "import result tail has more than one complete frame or oversize fragment",
                    ));
                }
                let frame: ImportResultFrame = strict_json(&bytes[..end])?;
                validate_event(&frame.event)?;
                if &frame.previous != previous
                    || frame.checksum
                        != Blake3Hash::digest(encoded(
                            &(&frame.previous, &frame.event),
                            MAX_IMPORT_RESULT_LINE_BYTES,
                        )?)
                {
                    return Err(conflict("import result frame chain differs"));
                }
                event_matches_pending(&frame.event, &progress)?;
                Ok(ImportResultTail {
                    frame: Some(frame),
                    next_offset: offset + (end + 1) as u64,
                    torn_tail: bytes.len() > end + 1,
                })
            }
        }
    }
    pub(crate) fn truncate_tail(
        &self,
        writer: &WriterPermit,
        known_offset: u64,
        expected_length: u64,
    ) -> Result<()> {
        writer.require_root(self.fs.root())?;
        let (progress, _) = self
            .load()?
            .ok_or_else(|| conflict("tail truncation requires known progress"))?;
        if progress.results_offset != known_offset
            || expected_length <= known_offset
            || expected_length - known_offset > MAX_IMPORT_RESULT_LINE_BYTES as u64
        {
            return Err(conflict(
                "tail truncation lacks its acknowledged bounded offset",
            ));
        }
        let (mut file, before) = self
            .open(&self.results, MAX_IMPORT_RESULTS_BYTES)?
            .ok_or_else(|| conflict("torn journal disappeared"))?;
        if before.len() != expected_length {
            return Err(conflict("torn journal length changed"));
        }
        file.seek(SeekFrom::Start(known_offset)).map_err(io_error)?;
        let mut bytes = Vec::new();
        (&mut file)
            .take(MAX_IMPORT_RESULT_LINE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(io_error)?;
        if bytes.len() as u64 != expected_length - known_offset || bytes.contains(&b'\n') {
            return Err(conflict("refuse to discard unknown complete result bytes"));
        }
        self.finish_read(&self.results, &file, &before)?;
        self.checked(&self.results)?;
        if fs::metadata(self.fs.root().resolve(&self.results)?)
            .map_err(io_error)?
            .len()
            != expected_length
        {
            return Err(conflict("torn length changed before truncation"));
        }
        self.fs.truncate_synced(&self.results, known_offset, writer)
    }
}
fn same_file(left: &Metadata, right: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        left.dev() == right.dev() && left.ino() == right.ino() && right.is_file() == left.is_file()
    }
    #[cfg(not(unix))]
    {
        let _ = (left, right);
        false
    }
}
fn timestamp(value: &str) -> Result<()> {
    time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .map(|_| ())
        .map_err(|_| conflict("import capture/change time is invalid"))
}
fn validate_change(change: &crate::changes::types::NamedChangeIdentity) -> Result<()> {
    timestamp(&change.created_at)
}
fn validate_manifest_summary(summary: &StoredImportManifest) -> Result<()> {
    if summary.items > MAX_IMPORT_ITEMS
        || summary.bytes == 0
        || summary.bytes > MAX_IMPORT_MANIFEST_BYTES
        || summary.first_item_offset == 0
        || summary.first_item_offset >= summary.bytes
    {
        return Err(conflict(
            "import manifest summary is outside its finite bound",
        ));
    }
    Ok(())
}
fn validate_result(result: &ImportGroupResult) -> Result<()> {
    let mut ids = std::collections::BTreeSet::new();
    if result.items.is_empty()
        || result.items.len() > 8
        || result.group >= MAX_IMPORT_ITEMS
        || !result
            .items
            .windows(2)
            .all(|items| items[0].ordinal.checked_add(1) == Some(items[1].ordinal))
        || result.items.iter().any(|item| {
            item.ordinal >= MAX_IMPORT_ITEMS
                || !ids.insert(&item.source_id)
                || !ids.insert(&item.revision_id)
        })
    {
        return Err(conflict(
            "import result lacks bounded contiguous unique identities",
        ));
    }
    Ok(())
}
fn validate_event(event: &ImportResultEvent) -> Result<()> {
    match event {
        ImportResultEvent::GroupCommitted { result } => validate_result(result),
        ImportResultEvent::AttemptClosed {
            group,
            change,
            prepared,
            ..
        } => {
            validate_change(change)?;
            if *group >= MAX_IMPORT_ITEMS
                || prepared
                    .as_ref()
                    .is_some_and(|prepared| prepared.change_id != change.change_id)
            {
                return Err(conflict("closed import attempt identity differs"));
            }
            Ok(())
        }
    }
}
fn event_matches_pending(event: &ImportResultEvent, progress: &ImportProgress) -> Result<()> {
    let pending = progress
        .pending
        .as_ref()
        .ok_or_else(|| conflict("unacknowledged result lacks its declared pending group"))?;
    let matches = match event {
        ImportResultEvent::GroupCommitted { result } => {
            result.group == pending.group
                && result.change.change_id == pending.change.change_id
                && result.items.len() == pending.captures.len()
                && result
                    .items
                    .iter()
                    .zip(&pending.captures)
                    .all(|(item, capture)| {
                        item.ordinal == capture.item.ordinal
                            && item.source_id == capture.allocation.source_id
                            && item.revision_id == capture.allocation.revision_id
                    })
        }
        ImportResultEvent::AttemptClosed { group, change, .. } => {
            *group == pending.group && change == &pending.change
        }
    };
    if !matches {
        return Err(conflict(
            "result event differs from declared pending mapping",
        ));
    }
    Ok(())
}
#[cfg(test)]
#[path = "source_import_state_tests.rs"]
mod tests;

//! Explicit bounded local manifests. These files declare inputs, not write authority.
use super::import_manifest_types::*;
use crate::{
    domain::{Blake3Hash, ErrorCode, Result, WikiError},
    vault::{DirectorySync, DurableIo, NativeIo},
};
use std::{
    fs::{self, File, Metadata, OpenOptions},
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
};

const BUFFER_BYTES: usize = 65_536;

fn failure(action: &str, error: impl std::fmt::Display) -> WikiError {
    WikiError::new(ErrorCode::Usage, format!("{action}: {error}"))
}
fn budget(message: &str) -> WikiError {
    WikiError::new(ErrorCode::BudgetExceeded, message)
}
fn digest(hasher: &blake3::Hasher) -> Result<Blake3Hash> {
    Blake3Hash::new(format!("blake3:{}", hasher.finalize().to_hex()))
}
fn same_file(left: &Metadata, right: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        left.dev() == right.dev() && left.ino() == right.ino()
    }
    #[cfg(not(unix))]
    {
        // No portable identity proof is available for owned-temp cleanup.
        let _ = (left, right);
        false
    }
}
fn open_regular(path: &Path) -> Result<(File, Metadata)> {
    let before = fs::symlink_metadata(path).map_err(|e| failure("inspect import file", e))?;
    if !before.is_file() || before.file_type().is_symlink() {
        return Err(failure(
            "import file",
            "requires a regular file, not a link",
        ));
    }
    let file = File::open(path).map_err(|e| failure("open import file", e))?;
    let opened = file
        .metadata()
        .map_err(|e| failure("inspect opened import file", e))?;
    let after = fs::symlink_metadata(path).map_err(|e| failure("recheck import file", e))?;
    if !opened.is_file()
        || !after.is_file()
        || after.file_type().is_symlink()
        || (cfg!(unix) && (!same_file(&before, &opened) || !same_file(&opened, &after)))
    {
        return Err(failure(
            "import file",
            "identity or type changed during open",
        ));
    }
    Ok((file, opened))
}
fn unchanged(file: &File, before: &Metadata) -> Result<()> {
    let after = file
        .metadata()
        .map_err(|e| failure("recheck import input", e))?;
    if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
        return Err(WikiError::new(
            ErrorCode::ContentConflict,
            "import input changed while reading",
        ));
    }
    Ok(())
}
fn utf8_path(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| failure("import path", "must be UTF-8"))
}
fn normalized_absolute(path: &Path) -> bool {
    path.is_absolute()
        && !path
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
        && path.components().collect::<PathBuf>().as_os_str() == path.as_os_str()
        && path.file_name().is_some()
}
fn text_candidate(path: &Path) -> bool {
    super::local_text::local_text_candidate(path)
}

fn item_metadata(item: &ImportManifestItem, ordinal: u64) -> Result<()> {
    if item.ordinal != ordinal || ordinal >= MAX_IMPORT_ITEMS {
        return Err(failure(
            "import item",
            "ordinal is not the next bounded item",
        ));
    }
    if !normalized_absolute(Path::new(&item.path))
        || item.path.contains('\0')
        || item.title.trim().is_empty()
    {
        return Err(failure(
            "import item",
            "requires an absolute normalized path and nonblank title",
        ));
    }
    if item.byte_len > MAX_IMPORT_ITEM_BYTES {
        return Err(budget("import item exceeds 64 MiB"));
    }
    if item.extraction == ImportExtraction::Utf8Preserve && !text_candidate(Path::new(&item.path)) {
        return Err(failure(
            "import extraction",
            "non-text format cannot declare UTF8-preserve",
        ));
    }
    Ok(())
}
fn add_input_bytes(total: &mut u64, bytes: u64) -> Result<()> {
    *total = total
        .checked_add(bytes)
        .filter(|sum| *sum <= MAX_IMPORT_INPUT_BYTES)
        .ok_or_else(|| budget("import original inputs exceed 64 GiB"))?;
    Ok(())
}

/// The bounds are checked before extending the one fixed-capacity line buffer.
fn line(reader: &mut BufReader<File>, offset: &mut u64, newline: bool) -> Result<Option<Vec<u8>>> {
    let mut bytes = Vec::with_capacity(MAX_IMPORT_LINE_BYTES);
    loop {
        let available = reader
            .fill_buf()
            .map_err(|e| failure("read import line", e))?;
        if available.is_empty() {
            if bytes.is_empty() {
                return Ok(None);
            }
            if newline {
                return Err(failure("import line", "missing final newline"));
            }
            return Ok(Some(bytes));
        }
        let end = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|index| index + 1);
        let take = end.unwrap_or(available.len());
        if bytes
            .len()
            .checked_add(take)
            .is_none_or(|size| size > MAX_IMPORT_LINE_BYTES)
        {
            return Err(budget("import JSONL line exceeds 64 KiB"));
        }
        let next = offset
            .checked_add(take as u64)
            .filter(|size| *size <= MAX_IMPORT_MANIFEST_BYTES)
            .ok_or_else(|| budget("import list/manifest exceeds 128 MiB"))?;
        bytes.extend_from_slice(&available[..take]);
        reader.consume(take);
        *offset = next;
        if end.is_some() {
            return Ok(Some(bytes));
        }
    }
}
struct BoundedLine(Vec<u8>);
impl Write for BoundedLine {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self
            .0
            .len()
            .checked_add(bytes.len())
            .is_none_or(|size| size >= MAX_IMPORT_LINE_BYTES)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "serialized import line exceeds limit",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn encoded(record: &ImportManifestRecord) -> Result<Vec<u8>> {
    let mut line = BoundedLine(Vec::with_capacity(MAX_IMPORT_LINE_BYTES));
    serde_json::to_writer(&mut line, record)
        .map_err(|_| budget("serialized import JSONL line exceeds 64 KiB"))?;
    line.0.push(b'\n');
    Ok(line.0)
}
fn emit(
    record: &ImportManifestRecord,
    output: &mut Option<&mut File>,
    body: &mut blake3::Hasher,
    all: &mut blake3::Hasher,
    offset: &mut u64,
    is_body: bool,
) -> Result<()> {
    let bytes = encoded(record)?;
    let next = offset
        .checked_add(bytes.len() as u64)
        .filter(|size| *size <= MAX_IMPORT_MANIFEST_BYTES)
        .ok_or_else(|| budget("import manifest exceeds 128 MiB"))?;
    if let Some(file) = output {
        file.write_all(&bytes)
            .map_err(|e| failure("write import manifest", e))?;
    }
    if is_body {
        body.update(&bytes);
    }
    all.update(&bytes);
    *offset = next;
    Ok(())
}

fn hash_input(path: &Path, remaining: u64) -> Result<(Blake3Hash, u64, bool)> {
    let (mut file, metadata) = open_regular(path)?;
    if metadata.len() > MAX_IMPORT_ITEM_BYTES || metadata.len() > remaining {
        return Err(budget("import input exceeds item or aggregate byte limit"));
    }
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; BUFFER_BYTES + 4];
    let (mut bytes, mut pending, mut valid) = (0u64, 0usize, true);
    loop {
        let count = file
            .read(&mut buffer[pending..pending + BUFFER_BYTES])
            .map_err(|e| failure("hash import input", e))?;
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(count as u64)
            .filter(|size| *size <= MAX_IMPORT_ITEM_BYTES && *size <= remaining)
            .ok_or_else(|| budget("import input grew beyond byte limits"))?;
        hasher.update(&buffer[pending..pending + count]);
        if valid {
            let length = pending + count;
            match std::str::from_utf8(&buffer[..length]) {
                Ok(_) => pending = 0,
                Err(error) if error.error_len().is_none() => {
                    let start = error.valid_up_to();
                    pending = length - start;
                    buffer.copy_within(start..length, 0);
                }
                Err(_) => {
                    valid = false;
                    pending = 0;
                }
            }
        }
    }
    unchanged(&file, &metadata)?;
    if bytes != metadata.len() {
        return Err(WikiError::new(
            ErrorCode::ContentConflict,
            "import input length changed",
        ));
    }
    Ok((digest(&hasher)?, bytes, valid && pending == 0))
}
struct OwnedTemp {
    path: PathBuf,
    file: File,
}
impl OwnedTemp {
    fn require_identity(&self, path: &Path) -> Result<()> {
        let named = fs::symlink_metadata(path).map_err(|e| failure("inspect owned manifest", e))?;
        let opened = self
            .file
            .metadata()
            .map_err(|e| failure("inspect complete manifest", e))?;
        if !named.is_file()
            || named.file_type().is_symlink()
            || (cfg!(unix) && !same_file(&named, &opened))
        {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "owned manifest identity changed",
            ));
        }
        Ok(())
    }
}
impl Drop for OwnedTemp {
    fn drop(&mut self) {
        if let (Ok(path), Ok(opened)) = (fs::symlink_metadata(&self.path), self.file.metadata())
            && path.is_file()
            && !path.file_type().is_symlink()
            && same_file(&path, &opened)
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}
fn output_path(output: &Path) -> Result<PathBuf> {
    let filename = output
        .file_name()
        .ok_or_else(|| failure("manifest output", "requires a filename"))?;
    if filename == "." || filename == ".." {
        return Err(failure("manifest output", "invalid filename"));
    }
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent =
        fs::canonicalize(parent).map_err(|e| failure("resolve manifest output directory", e))?;
    let output = parent.join(filename);
    utf8_path(&output)?;
    match fs::symlink_metadata(&output) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(output),
        Ok(_) => Err(WikiError::new(
            ErrorCode::ContentConflict,
            "manifest output already exists",
        )),
        Err(error) => Err(failure("inspect manifest output", error)),
    }
}
fn temporary(output: &Path) -> Result<OwnedTemp> {
    let path = output
        .parent()
        .unwrap()
        .join(format!(".lwiki-import-{}.tmp", uuid::Uuid::now_v7()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(&path)
        .map_err(|e| failure("create owned import temporary", e))?;
    Ok(OwnedTemp { path, file })
}

pub(crate) fn prepare_manifest(
    input_list: &Path,
    output: &Path,
    dry_run: bool,
) -> Result<ManifestPreparation> {
    let (input, metadata) = open_regular(input_list)?;
    if metadata.len() > MAX_IMPORT_MANIFEST_BYTES {
        return Err(budget("import input list exceeds 128 MiB"));
    }
    let list_path = fs::canonicalize(input_list).map_err(|e| failure("resolve input list", e))?;
    utf8_path(&list_path)?;
    let resolved =
        fs::symlink_metadata(&list_path).map_err(|e| failure("inspect resolved input list", e))?;
    if !resolved.is_file()
        || resolved.file_type().is_symlink()
        || (cfg!(unix) && !same_file(&metadata, &resolved))
    {
        return Err(WikiError::new(
            ErrorCode::ContentConflict,
            "input list identity changed while resolving its base",
        ));
    }
    let output = output_path(output)?;
    let mut owned = if dry_run {
        None
    } else {
        Some(temporary(&output)?)
    };
    let mut sink = owned.as_mut().map(|temp| &mut temp.file);
    let mut reader = BufReader::with_capacity(BUFFER_BYTES, input);
    let (mut body, mut all) = (blake3::Hasher::new(), blake3::Hasher::new());
    let (mut source_offset, mut manifest_bytes, mut items, mut input_bytes) =
        (0u64, 0u64, 0u64, 0u64);
    emit(
        &ImportManifestRecord::Header {
            version: IMPORT_MANIFEST_VERSION,
        },
        &mut sink,
        &mut body,
        &mut all,
        &mut manifest_bytes,
        true,
    )?;
    let first_item_offset = manifest_bytes;
    while let Some(bytes) = line(&mut reader, &mut source_offset, false)? {
        if items >= MAX_IMPORT_ITEMS {
            return Err(budget("import list exceeds 200000 items"));
        }
        let input: ImportInput =
            serde_json::from_slice(&bytes).map_err(|e| failure("parse import input line", e))?;
        let path = Path::new(&input.path);
        let path = if path.is_absolute() {
            path.to_owned()
        } else {
            list_path.parent().unwrap().join(path)
        };
        // Refuse a link as the named input before canonicalization can erase it.
        let info = fs::symlink_metadata(&path).map_err(|e| failure("inspect source input", e))?;
        if !info.is_file() || info.file_type().is_symlink() {
            return Err(failure("source input", "requires a regular local file"));
        }
        let path = fs::canonicalize(path).map_err(|e| failure("resolve source input", e))?;
        let resolved =
            fs::symlink_metadata(&path).map_err(|e| failure("inspect resolved source input", e))?;
        if !resolved.is_file()
            || resolved.file_type().is_symlink()
            || (cfg!(unix) && !same_file(&info, &resolved))
        {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "source input identity changed while resolving",
            ));
        }
        let title = input
            .title
            .unwrap_or(utf8_path(Path::new(path.file_name().unwrap()))?);
        let (hash, byte_len, utf8) = hash_input(&path, MAX_IMPORT_INPUT_BYTES - input_bytes)?;
        add_input_bytes(&mut input_bytes, byte_len)?;
        let extraction = if text_candidate(&path) && utf8 {
            ImportExtraction::Utf8Preserve
        } else {
            ImportExtraction::Unsupported
        };
        let item = ImportManifestItem {
            ordinal: items,
            path: utf8_path(&path)?,
            title,
            media_type: input.media_type,
            original_hash: hash,
            byte_len,
            extraction,
        };
        item_metadata(&item, items)?;
        emit(
            &ImportManifestRecord::Item { item },
            &mut sink,
            &mut body,
            &mut all,
            &mut manifest_bytes,
            true,
        )?;
        items += 1;
    }
    unchanged(reader.get_ref(), &metadata)?;
    let footer = ImportManifestRecord::Footer {
        items,
        body_hash: digest(&body)?,
    };
    emit(
        &footer,
        &mut sink,
        &mut body,
        &mut all,
        &mut manifest_bytes,
        false,
    )?;
    if let Some(owned) = &owned {
        owned
            .file
            .sync_all()
            .map_err(|e| failure("sync complete import manifest", e))?;
        owned.require_identity(&owned.path)?;
        fs::hard_link(&owned.path, &output).map_err(|e| {
            if e.kind() == io::ErrorKind::AlreadyExists {
                WikiError::new(
                    ErrorCode::ContentConflict,
                    "manifest output became occupied",
                )
            } else {
                failure("publish import manifest without overwrite", e)
            }
        })?;
        owned.require_identity(&output)?;
        if NativeIo
            .sync_directory(output.parent().unwrap())
            .map_err(|e| failure("sync manifest output directory", e))?
            == DirectorySync::Unsupported
        {
            return Err(WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "manifest output directory durability unsupported",
            ));
        }
    }
    Ok(ManifestPreparation {
        path: utf8_path(&output)?,
        manifest_hash: digest(&all)?,
        items,
        input_bytes,
        manifest_bytes,
        first_item_offset,
        preview: dry_run,
    })
}

fn manifest_reader(path: &Path) -> Result<(BufReader<File>, Metadata)> {
    let (file, metadata) = open_regular(path)?;
    if metadata.len() > MAX_IMPORT_MANIFEST_BYTES {
        return Err(budget("manifest exceeds 128 MiB"));
    }
    Ok((BufReader::with_capacity(BUFFER_BYTES, file), metadata))
}
fn record(bytes: &[u8]) -> Result<ImportManifestRecord> {
    serde_json::from_slice(bytes).map_err(|e| failure("parse manifest line", e))
}
pub(crate) fn validate_manifest(path: &Path) -> Result<ManifestPreparation> {
    let (mut reader, metadata) = manifest_reader(path)?;
    let mut offset = 0;
    let header = line(&mut reader, &mut offset, true)?
        .ok_or_else(|| failure("manifest", "missing header"))?;
    if record(&header)?
        != (ImportManifestRecord::Header {
            version: IMPORT_MANIFEST_VERSION,
        })
    {
        return Err(failure("manifest", "unsupported or missing first header"));
    }
    let first_item_offset = offset;
    let (mut body, mut all) = (blake3::Hasher::new(), blake3::Hasher::new());
    body.update(&header);
    all.update(&header);
    let (mut items, mut input_bytes) = (0, 0);
    loop {
        let bytes = line(&mut reader, &mut offset, true)?
            .ok_or_else(|| failure("manifest", "missing footer"))?;
        all.update(&bytes);
        match record(&bytes)? {
            ImportManifestRecord::Item { item } => {
                item_metadata(&item, items)?;
                add_input_bytes(&mut input_bytes, item.byte_len)?;
                body.update(&bytes);
                items += 1;
            }
            ImportManifestRecord::Footer {
                items: count,
                body_hash,
            } => {
                if count != items || body_hash != digest(&body)? {
                    return Err(failure(
                        "manifest footer",
                        "item count or exact body hash differs",
                    ));
                }
                if offset != metadata.len() {
                    return Err(failure("manifest", "bytes follow footer"));
                }
                break;
            }
            ImportManifestRecord::Header { .. } => {
                return Err(failure("manifest", "repeated header"));
            }
        }
    }
    unchanged(reader.get_ref(), &metadata)?;
    Ok(ManifestPreparation {
        path: utf8_path(&fs::canonicalize(path).map_err(|e| failure("resolve manifest", e))?)?,
        manifest_hash: digest(&all)?,
        items,
        input_bytes,
        manifest_bytes: offset,
        first_item_offset,
        preview: false,
    })
}

/// Caller first validates and pins the entire manifest. This bounded cursor read
/// checks local order/metadata/footer, never reconstructs the prefix body hash.
pub(crate) fn read_manifest_batch(
    path: &Path,
    offset: u64,
    next_ordinal: u64,
    max_items: usize,
) -> Result<ManifestBatch> {
    if !(1..=8).contains(&max_items) || next_ordinal > MAX_IMPORT_ITEMS {
        return Err(failure(
            "manifest batch",
            "requires 1..8 items and bounded ordinal",
        ));
    }
    let (mut reader, metadata) = manifest_reader(path)?;
    if offset == 0 || offset >= metadata.len() {
        return Err(failure(
            "manifest batch",
            "offset must precede an explicit record/footer",
        ));
    }
    reader
        .seek(SeekFrom::Start(offset - 1))
        .map_err(|e| failure("seek manifest boundary", e))?;
    let mut byte = [0u8; 1];
    reader
        .read_exact(&mut byte)
        .map_err(|e| failure("read manifest boundary", e))?;
    if byte != [b'\n'] {
        return Err(failure("manifest batch", "offset is not a line boundary"));
    }
    let mut next_offset = offset;
    let mut items = Vec::with_capacity(max_items);
    let mut eof = false;
    while items.len() < max_items {
        let bytes = line(&mut reader, &mut next_offset, true)?
            .ok_or_else(|| failure("manifest batch", "missing explicit footer"))?;
        match record(&bytes)? {
            ImportManifestRecord::Item { item } => {
                item_metadata(&item, next_ordinal + items.len() as u64)?;
                items.push(item);
            }
            ImportManifestRecord::Footer { items: count, .. } => {
                if count != next_ordinal + items.len() as u64 || next_offset != metadata.len() {
                    return Err(failure("manifest batch", "footer count or EOF differs"));
                }
                eof = true;
                break;
            }
            ImportManifestRecord::Header { .. } => {
                return Err(failure("manifest batch", "header is not an item cursor"));
            }
        }
    }
    unchanged(reader.get_ref(), &metadata)?;
    Ok(ManifestBatch {
        items,
        next_offset,
        eof,
    })
}

#[cfg(test)]
#[path = "import_manifest_tests.rs"]
mod tests;

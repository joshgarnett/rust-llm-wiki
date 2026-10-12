//! Optional, bounded navigation observations. Immutable citations remain the proof.
use super::{ContextPassage, SourceReadLink, SourceReadLinkStatus, verification::Meter};
use crate::{
    catalog::{Catalog, DocumentRow, RecordRow},
    domain::{
        Blake3Hash, CitationRef, RecordId, RecordKind, VaultRelativePath,
        source_presentation::SourcePresentation,
    },
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::{Duration, Instant},
};

const MAX_REVISIONS: usize = 32;
const MAX_FILE_BYTES: usize = 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 4 * 1024 * 1024;
type Key = (RecordId, RecordId);

#[derive(Debug, Clone, Default)]
pub(crate) struct VerifiedSourceLinks(BTreeMap<Key, SourceReadLink>);
impl VerifiedSourceLinks {
    pub(crate) fn get(&self, source: &RecordId, revision: &RecordId) -> Option<SourceReadLink> {
        self.0.get(&(source.clone(), revision.clone())).cloned()
    }
    pub(crate) fn decorate(&self, passage: &mut ContextPassage) {
        passage.source_links.clear();
        for citation in &passage.citations {
            let (source, revision) = match citation {
                CitationRef::Source(reference) => {
                    (&reference.source_id, &reference.source_revision)
                }
                CitationRef::Assertion(reference) => {
                    (&reference.source_id, &reference.source_revision)
                }
            };
            if let Some(link) = self.get(source, revision)
                && !passage.source_links.contains(&link)
            {
                passage.source_links.push(link);
            }
        }
    }
}
struct Witness {
    path: VaultRelativePath,
    hash: Blake3Hash,
    absolute: std::path::PathBuf,
    byte_length: usize,
}
pub(crate) struct SourceLinkProof {
    pub(crate) links: VerifiedSourceLinks,
    witnesses: BTreeMap<Key, Witness>,
    reserve: (usize, usize, usize),
    bytes: usize,
    retained_bytes: usize,
    deadline: Instant,
}
impl SourceLinkProof {
    pub(crate) fn empty() -> Self {
        Self {
            links: VerifiedSourceLinks::default(),
            witnesses: BTreeMap::new(),
            reserve: (0, 0, 0),
            bytes: 0,
            retained_bytes: 0,
            deadline: Instant::now(),
        }
    }
    /// Caller supplies only authenticated rows, never a raw discovery catalog.
    pub(crate) fn observe<'a>(
        catalog: &Catalog,
        records: &BTreeMap<RecordId, RecordRow>,
        documents: impl IntoIterator<Item = &'a DocumentRow>,
        meter: &mut Meter,
        reserve: (usize, usize, usize),
    ) -> Self {
        let mut proof = Self {
            links: VerifiedSourceLinks::default(),
            witnesses: BTreeMap::new(),
            reserve,
            bytes: 0,
            retained_bytes: 0,
            deadline: Instant::now() + Duration::from_millis(meter.remaining_ms() / 2),
        };
        let mut attempted = BTreeSet::new();
        for document in documents {
            let (Some(source_id), Some(revision_id)) =
                (&document.source_id, &document.owner_revision)
            else {
                continue;
            };
            let key = (source_id.clone(), revision_id.clone());
            if attempted.contains(&key) {
                continue;
            }
            if attempted.len() == MAX_REVISIONS {
                break;
            }
            attempted.insert(key.clone());
            let declaration = (|| {
                let source = records.get(source_id)?;
                let revision = records.get(revision_id)?;
                if source.record.kind() != RecordKind::Source
                    || source.record.id() != source_id
                    || revision.record.kind() != RecordKind::Revision
                    || revision.record.id() != revision_id
                    || revision.record.string("wiki_source_id") != Some(source_id.as_str())
                    || revision.record.string("wiki_content_hash") != Some(document.hash.as_str())
                {
                    return None;
                }
                let metadata = SourcePresentation::from_record(&source.record).ok()??;
                metadata.validate_owner(&source.record).ok()?;
                let current = &metadata.current.revision_id == revision_id;
                let (path, hash) = if current {
                    if metadata.current.content_hash.as_ref() != Some(&document.hash) {
                        return None;
                    }
                    (
                        metadata.current.path.clone(),
                        metadata.current.generated_hash.clone(),
                    )
                } else {
                    let saved = metadata.published.get(revision_id)?.as_ref()?;
                    (saved.path.clone(), saved.generated_hash.clone())
                };
                let status = if source.record.string("wiki_status") == Some("withdrawn") {
                    SourceReadLinkStatus::Withdrawn
                } else if current {
                    SourceReadLinkStatus::Current
                } else {
                    SourceReadLinkStatus::Historical
                };
                Some((path, hash, status, source.record.title().to_owned()))
            })();
            let Some((path, hash, status, title)) = declaration else {
                continue;
            };
            // Keep each admitted witness's second read within the local cap too.
            let limit = MAX_FILE_BYTES.min(
                MAX_TOTAL_BYTES
                    .saturating_sub(proof.bytes)
                    .saturating_sub(proof.retained_bytes)
                    / 2,
            );
            let Some((bytes, absolute)) = proof.read(catalog, &path, meter, limit) else {
                continue;
            };
            if Blake3Hash::digest(&bytes) != hash {
                continue;
            }
            let Some(markdown) = markdown_link(&title, &absolute) else {
                continue;
            };
            proof.links.0.insert(
                key.clone(),
                SourceReadLink {
                    source_id: source_id.clone(),
                    revision_id: revision_id.clone(),
                    status,
                    markdown,
                },
            );
            proof.retained_bytes += bytes.len();
            proof.witnesses.insert(
                key,
                Witness {
                    path,
                    hash,
                    absolute,
                    byte_length: bytes.len(),
                },
            );
        }
        proof
    }
    fn read(
        &mut self,
        catalog: &Catalog,
        path: &VaultRelativePath,
        meter: &mut Meter,
        byte_limit: usize,
    ) -> Option<(Vec<u8>, std::path::PathBuf)> {
        let before = meter.work().0;
        let remaining_ms = self
            .deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(u64::MAX as u128) as u64;
        let result = meter.read_optional(
            catalog,
            path,
            byte_limit.min(MAX_TOTAL_BYTES.saturating_sub(self.bytes)),
            self.reserve,
            remaining_ms,
        );
        self.bytes += meter.work().0 - before;
        result
    }
    /// A failed optional witness removes navigation only, before canonical sealing.
    pub(crate) fn recheck(&mut self, catalog: &Catalog, meter: &mut Meter) -> bool {
        let witnesses = std::mem::take(&mut self.witnesses);
        let mut removed = false;
        for (key, witness) in witnesses {
            let valid = self
                .read(catalog, &witness.path, meter, witness.byte_length)
                .is_some_and(|(bytes, absolute)| {
                    absolute == witness.absolute && Blake3Hash::digest(&bytes) == witness.hash
                });
            if !valid {
                self.links.0.remove(&key);
                removed = true;
            }
        }
        removed
    }
}

fn markdown_link(title: &str, absolute: &Path) -> Option<String> {
    if !absolute.is_absolute() {
        return None;
    }
    let mut label = String::new();
    for ch in title.chars() {
        if ch.is_control() {
            label.push(' ');
            continue;
        }
        if matches!(
            ch,
            '\\' | '[' | ']' | '(' | ')' | '<' | '>' | '*' | '_' | '`' | '#' | '!' | '|'
        ) {
            label.push('\\');
        }
        label.push(ch);
    }
    // Encode once from filesystem bytes, preserving only URI path delimiters.
    // Non-UTF-8 paths are omitted rather than producing a lossy wrong target.
    #[cfg(not(windows))]
    let path = absolute.to_str()?.to_owned();
    #[cfg(windows)]
    let path = {
        let prefix = match absolute.components().next()? {
            std::path::Component::Prefix(prefix) => prefix,
            _ => return None,
        };
        let tail = absolute.strip_prefix(prefix.as_os_str()).ok()?.to_str()?;
        windows_destination(prefix.kind(), tail)?
    };
    let target = encode_destination(&path);
    Some(format!("[{label}](<{target}>)"))
}

fn encode_destination(path: &str) -> String {
    let mut target = String::new();
    for byte in path.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b':' | b'-' | b'_' | b'.' | b'~') {
            target.push(*byte as char);
        } else {
            use std::fmt::Write;
            // Writing into String cannot fail.
            let _ = write!(&mut target, "%{byte:02X}");
        }
    }
    target
}

/// Use native Prefix parsing for observed Windows paths, with one pure formatter
/// also exercised on Unix. Proof witnesses retain the original native path.
#[cfg(any(windows, test))]
fn windows_destination(prefix: std::path::Prefix<'_>, tail: &str) -> Option<String> {
    use std::path::Prefix;
    if !tail.is_empty() && !tail.starts_with(['\\', '/']) {
        return None;
    }
    let tail = tail.replace('\\', "/");
    match prefix {
        Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) if drive.is_ascii_alphabetic() => {
            Some(format!(
                "{}:{}",
                char::from(drive),
                if tail.is_empty() { "/" } else { &tail }
            ))
        }
        Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
            let server = server.to_str()?;
            let share = share.to_str()?;
            if server.is_empty()
                || share.is_empty()
                || server.contains(['\\', '/'])
                || share.contains(['\\', '/'])
            {
                return None;
            }
            Some(format!("//{server}/{share}{tail}"))
        }
        // Device namespace and generic verbatim paths have no supported local
        // Markdown destination. Do not guess a drive or network share.
        _ => None,
    }
}

#[cfg(test)]
#[path = "source_links_tests.rs"]
mod tests;

//! Preserve-all fixed-layout migration; the pending epoch survives until publication.
use super::{
    layout::{self, Layout, LayoutKind, RawReader},
    types::*,
};
use crate::{
    catalog::Catalog,
    changes::{ChangeEngine, ChangeStatus},
    domain::*,
    records::{LinkResolution, LinkSyntax, ParsedNote, RegistryEntry, edit_note, parse_note},
    sources::{
        CitationState,
        presentation::{self, SourceNavigation},
        presentation_planning,
    },
    vault::{ExpectedState, VaultFs, VaultRoot, WriterPermit, operational::RunStore},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};
pub(crate) const PENDING: &str = ".wiki/state/storage/source-layout.json";
const MAX_RECEIPT: usize = 64 * 1024 * 1024;
const MAX_BOOTSTRAP: usize = 1024 * 1024;
const MAX_FILE: usize = crate::changes::prepare::MAX_PAYLOAD_BYTES;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Retention {
    PreserveAll,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Copy {
    source: VaultRelativePath,
    destination: VaultRelativePath,
    hash: Blake3Hash,
    bytes: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Replacement {
    path: VaultRelativePath,
    before: Option<String>,
    after: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Epoch {
    version: u32,
    bootstrap: bool,
    retention: Retention,
    id: RecordId,
    vault_id: RecordId,
    from_layout: u32,
    marker_original: String,
    marker_after: String,
    old_activation: Option<Vec<u8>>,
    files: Vec<StorageFile>,
    copies: Vec<Copy>,
    replacements: Vec<Replacement>,
    plan: SourceLayoutPlan,
}
struct Analysis {
    marker_original: String,
    marker_after: String,
    files: Vec<StorageFile>,
    copies: Vec<Copy>,
    replacements: Vec<Replacement>,
    plan: SourceLayoutPlan,
}
fn rel(value: impl Into<String>) -> Result<VaultRelativePath> {
    VaultRelativePath::new(value)
}
fn recovery(message: impl Into<String>) -> WikiError {
    WikiError::new(ErrorCode::RecoveryRequired, message)
}
fn conflict(message: impl Into<String>) -> WikiError {
    WikiError::new(ErrorCode::ContentConflict, message)
}
fn budget(message: impl Into<String>) -> WikiError {
    WikiError::new(ErrorCode::BudgetExceeded, message)
}
fn options(value: &SourceLayoutOptions) -> StorageOptions {
    StorageOptions {
        max_files: value.max_files,
        max_bytes: value.max_bytes,
        expected_plan: None,
        ..StorageOptions::default()
    }
}
fn archive(id: &RecordId) -> Result<VaultRelativePath> {
    rel(format!(".wiki/state/storage/receipts/{id}.plan.json"))
}
fn backup_root(id: &RecordId) -> String {
    format!(".wiki/retained/source-layout-backups/{id}")
}
fn backup_path(id: &RecordId, path: &VaultRelativePath) -> Result<VaultRelativePath> {
    // Keep the exact marker outside the mirrored tree so existing nested-vault
    // containment guards remain effective for every backup path.
    if path.as_str() == "WIKI.md" {
        rel(format!("{}/marker/WIKI.before", backup_root(id)))
    } else {
        rel(format!("{}/vault/{path}", backup_root(id)))
    }
}
fn utf8(bytes: Vec<u8>) -> Result<String> {
    String::from_utf8(bytes).map_err(|_| WikiError::invalid("migration Markdown is not UTF-8"))
}
fn read_exact(root: &VaultRoot, file: &StorageFile) -> Result<Vec<u8>> {
    let limit =
        usize::try_from(file.bytes).map_err(|_| budget("migration file exceeds platform"))?;
    if limit > MAX_FILE {
        return Err(budget(format!(
            "migration file {} exceeds the existing 64 MiB file limit",
            file.path
        )));
    }
    let bytes = layout::raw_read(root, &file.path, limit)?
        .ok_or_else(|| conflict(format!("migration input {} disappeared", file.path)))?;
    if bytes.len() as u64 != file.bytes || Blake3Hash::digest(&bytes) != file.hash {
        return Err(conflict(format!("migration input {} changed", file.path)));
    }
    Ok(bytes)
}
fn validate_epoch(epoch: &Epoch) -> Result<()> {
    if epoch.version != 1
        || !matches!(epoch.from_layout, 1 | 2)
        || epoch.files.len() > 1_000_000
        || epoch.copies.len() > 1_000_000
        || epoch.replacements.len() > 1_000_000
        || epoch.plan.to_layout != 3
        || epoch.plan.vault_id != epoch.vault_id
        || epoch.plan.from_layout != epoch.from_layout
        || !epoch.plan.complete
        || !epoch.plan.blockers.is_empty()
    {
        return Err(recovery(
            "source-layout epoch identity, version or bounds differ",
        ));
    }
    let before = parse_note(epoch.marker_original.as_bytes());
    let after = parse_note(epoch.marker_after.as_bytes());
    let old = before
        .canonical
        .as_ref()
        .ok_or_else(|| recovery("original migration marker invalid"))?;
    let new = after
        .canonical
        .as_ref()
        .ok_or_else(|| recovery("proposed migration marker invalid"))?;
    if old.kind() != RecordKind::Vault
        || old.id() != &epoch.vault_id
        || old.string("wiki_schema") != Some(epoch.from_layout.to_string().as_str())
        || new.kind() != RecordKind::Vault
        || new.id() != old.id()
        || new.string("wiki_schema") != Some("3")
        || old
            .fields()
            .iter()
            .filter(|(key, _)| key.as_str() != "wiki_schema")
            .ne(new
                .fields()
                .iter()
                .filter(|(key, _)| key.as_str() != "wiki_schema"))
    {
        return Err(recovery(
            "migration marker changes vault identity or fields",
        ));
    }
    if epoch.bootstrap
        && (!epoch.files.is_empty()
            || !epoch.copies.is_empty()
            || !epoch.replacements.is_empty()
            || epoch.from_layout != 1
            || before.body() != after.body()
            || epoch.old_activation.is_some()
            || serde_json::to_value(&epoch.plan).map_err(|e| recovery(e.to_string()))?
                != serde_json::to_value(empty_plan(epoch.vault_id.clone(), 1))
                    .map_err(|e| recovery(e.to_string()))?
            || crate::records::edit::migrate_schema(&before, "3", &before.source_hash)?
                != epoch.marker_after.as_bytes())
    {
        return Err(recovery("bootstrap contains migration actions"));
    }
    let mut destinations = BTreeSet::new();
    for copy in &epoch.copies {
        let logical =
            layout::logical_path_for(LayoutKind::from_version(epoch.from_layout)?, &copy.source)
                .unwrap_or_else(|| copy.source.clone());
        if layout::managed_path_for(LayoutKind::ReadableSources, &logical).as_ref()
            != Some(&copy.destination)
            || copy.source == copy.destination
            || copy.bytes > MAX_FILE as u64
            || !destinations.insert(copy.destination.clone())
            || !epoch.files.iter().any(|file| {
                file.path == copy.source && file.hash == copy.hash && file.bytes == copy.bytes
            })
        {
            return Err(recovery(
                "migration copy leaves fixed mapping or original inventory",
            ));
        }
    }
    destinations.clear();
    for item in &epoch.replacements {
        if !destinations.insert(item.path.clone())
            || item.after.len() > MAX_FILE
            || item
                .before
                .as_ref()
                .is_some_and(|before| before.len() > MAX_FILE)
            || item.path.as_str() == "WIKI.md"
            || item.path.as_str().starts_with(".obsidian/")
        {
            return Err(recovery(
                "migration replacement exceeds bounds or changes editor settings",
            ));
        }
    }
    Ok(())
}
pub(crate) fn verify_activation_with_reader(
    active: &Layout,
    reader: &mut RawReader<'_>,
) -> Result<()> {
    let bytes = layout::read_with_reader(reader, &archive(&active.migration_id)?, MAX_RECEIPT)?
        .ok_or_else(|| {
            recovery("source-layout activation lost its receipt; restore the complete wiki backup")
        })?;
    if active.version != 3 || Blake3Hash::digest(&bytes) != active.migration_hash {
        return Err(recovery("source-layout receipt hash differs"));
    }
    let epoch: Epoch = layout::decode(&bytes)?;
    validate_epoch(&epoch)?;
    if epoch.id != active.migration_id || epoch.vault_id != active.vault_id {
        return Err(recovery(
            "source-layout receipt belongs to another migration",
        ));
    }
    Ok(())
}
pub(crate) fn pending_activation_matches_with_reader(reader: &mut RawReader<'_>) -> Result<bool> {
    let Some(bytes) = layout::read_with_reader(reader, &rel(PENDING)?, MAX_RECEIPT)? else {
        return Ok(false);
    };
    let epoch: Epoch = layout::decode(&bytes)?;
    validate_epoch(&epoch)?;
    let archived = layout::read_with_reader(reader, &archive(&epoch.id)?, MAX_RECEIPT)?;
    let marker = layout::read_with_reader(reader, &rel("WIKI.md")?, 1024 * 1024)?;
    let active = layout::read_with_reader(reader, &rel(layout::ACTIVE)?, 4096)?;
    Ok(archived.as_ref() == Some(&bytes)
        && marker.as_deref() == Some(epoch.marker_after.as_bytes())
        && active == epoch.old_activation)
}
fn empty_plan(id: RecordId, from: u32) -> SourceLayoutPlan {
    SourceLayoutPlan {
        vault_id: id,
        plan_hash: Blake3Hash::digest(b"source-layout-3-active"),
        from_layout: from,
        to_layout: 3,
        complete: true,
        sources: 0,
        historical_copies: 0,
        authored_link_edits: 0,
        planned_copy_bytes: 0,
        retained_backup_bytes: 0,
        rendered_bytes: 0,
        required_free_bytes: 0,
        blockers: vec![],
        warnings: vec![],
    }
}
/// The updated wiki marker and its authority files.
type BootstrapFiles = (Vec<u8>, Vec<(VaultRelativePath, Vec<u8>)>);

/// The initializer writes these authority files durably before its first marker.
pub(crate) fn bootstrap_files(marker: &[u8]) -> Result<BootstrapFiles> {
    let note = parse_note(marker);
    let record = note
        .canonical
        .as_ref()
        .ok_or_else(|| WikiError::invalid("bootstrap marker invalid"))?;
    let after = crate::records::edit::migrate_schema(&note, "3", &note.source_hash)?;
    let id = RecordId::new(format!("source_layout_{}", uuid::Uuid::now_v7()))?;
    let epoch = Epoch {
        version: 1,
        bootstrap: true,
        retention: Retention::PreserveAll,
        id: id.clone(),
        vault_id: record.id().clone(),
        from_layout: 1,
        marker_original: utf8(marker.to_vec())?,
        marker_after: utf8(after.clone())?,
        old_activation: None,
        files: vec![],
        copies: vec![],
        replacements: vec![],
        plan: empty_plan(record.id().clone(), 1),
    };
    validate_epoch(&epoch)?;
    let receipt = layout::encode(&epoch)?;
    if receipt.len() > MAX_BOOTSTRAP {
        return Err(budget("initialization receipt exceeds 1 MiB"));
    }
    let active = Layout {
        version: 3,
        vault_id: record.id().clone(),
        migration_id: id.clone(),
        migration_hash: Blake3Hash::digest(&receipt),
    };
    Ok((
        after,
        vec![
            (archive(&id)?, receipt),
            (rel(layout::ACTIVE)?, layout::encode(&active)?),
        ],
    ))
}
/// Recover only an entirely published bootstrap authority directory. It is
/// atomically installed before the first marker, and has no user-editable data.
pub(crate) fn resume_bootstrap_marker(
    root: &VaultRoot,
    title: Option<&str>,
) -> Result<Option<Vec<u8>>> {
    if layout::raw_read(root, &rel("WIKI.md")?, MAX_FILE)?.is_some() {
        return Ok(None);
    }
    let Some(active_bytes) = layout::raw_read(root, &rel(layout::ACTIVE)?, 4096)? else {
        return Ok(None);
    };
    let active: Layout = layout::decode(&active_bytes)?;
    let receipt_path = archive(&active.migration_id)?;
    let receipt = layout::raw_read(root, &receipt_path, MAX_BOOTSTRAP)?
        .ok_or_else(|| recovery("initialization receipt is missing"))?;
    let epoch: Epoch = layout::decode(&receipt)?;
    validate_epoch(&epoch)?;
    if !epoch.bootstrap
        || active.version != 3
        || active.vault_id != epoch.vault_id
        || active.migration_id != epoch.id
        || active.migration_hash != Blake3Hash::digest(&receipt)
    {
        return Err(recovery(
            "unmarked working folder is not an exact empty bootstrap",
        ));
    }
    if parse_note(epoch.marker_after.as_bytes())
        .canonical
        .as_ref()
        .is_none_or(|record| title.is_some_and(|title| record.title() != title))
    {
        return Err(conflict(
            "interrupted initialization used a different wiki title; retry its original setup command",
        ));
    }
    let expected = BTreeMap::from([
        (rel(layout::ACTIVE)?, active_bytes),
        (receipt_path, receipt),
        (rel(".wiki/.gitignore")?, b"cache/\n".to_vec()),
    ]);
    let directories = BTreeSet::from([
        ".wiki",
        ".wiki/state",
        ".wiki/state/storage",
        ".wiki/state/storage/receipts",
    ]);
    let mut pending = vec![rel(".wiki")?];
    let mut seen = BTreeSet::new();
    let mut steps = 0;
    while let Some(directory) = pending.pop() {
        for entry in
            std::fs::read_dir(root.resolve_raw(&directory)?).map_err(|e| recovery(e.to_string()))?
        {
            steps += 1;
            if steps > 16 {
                return Err(recovery("unmarked bootstrap has additional entries"));
            }
            let entry = entry.map_err(|e| recovery(e.to_string()))?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| recovery("bootstrap filename is not UTF-8"))?;
            let path = rel(format!("{directory}/{name}"))?;
            let kind = entry.file_type().map_err(|e| recovery(e.to_string()))?;
            if kind.is_dir() && directories.contains(path.as_str()) {
                pending.push(path);
            } else if kind.is_file() && expected.contains_key(&path) {
                if layout::raw_read(root, &path, MAX_BOOTSTRAP)?.as_ref() != expected.get(&path) {
                    return Err(recovery(format!("bootstrap bytes changed at {path}")));
                }
                seen.insert(path);
            } else {
                return Err(recovery(format!(
                    "unmarked bootstrap contains unfamiliar data at {path}; every file was preserved"
                )));
            }
        }
    }
    if seen.len() != expected.len() {
        return Err(recovery("unmarked bootstrap is incomplete"));
    }
    for (index, entry) in std::fs::read_dir(root.path())
        .map_err(|e| recovery(e.to_string()))?
        .enumerate()
    {
        if index >= 65_536 {
            return Err(budget("initialization root exceeds entry bound"));
        }
        let entry = entry.map_err(|e| recovery(e.to_string()))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| recovery("initialization filename is not UTF-8"))?;
        let folded = unicase::UniCase::unicode(name.as_str()).to_folded_case();
        if ["wiki.md", "pages", "knowledge", "sources", "changes"].contains(&folded.as_str())
            || folded == ".wiki" && name != ".wiki"
        {
            return Err(conflict(format!(
                "interrupted initialization preserved occupied reserved path {name}"
            )));
        }
    }
    Ok(Some(epoch.marker_after.into_bytes()))
}
pub(crate) fn completed_bootstrap_identity(
    root: &VaultRoot,
    title: Option<&str>,
) -> Result<Option<RecordId>> {
    let Some(marker) = layout::raw_read(root, &rel("WIKI.md")?, MAX_BOOTSTRAP)? else {
        return Ok(None);
    };
    let note = parse_note(&marker);
    if note.canonical.as_ref().is_none_or(|record| {
        record.kind() != RecordKind::Vault || record.string("wiki_schema") != Some("3")
    }) {
        return Ok(None);
    }
    if layout::layout_kind(root)? != LayoutKind::ReadableSources {
        return Ok(None);
    }
    let active: Layout = layout::decode(
        &layout::raw_read(root, &rel(layout::ACTIVE)?, 4096)?
            .ok_or_else(|| recovery("bootstrap activation is missing"))?,
    )?;
    let bytes = layout::raw_read(root, &archive(&active.migration_id)?, MAX_RECEIPT)?
        .ok_or_else(|| recovery("bootstrap receipt is missing"))?;
    let epoch: Epoch = layout::decode(&bytes)?;
    validate_epoch(&epoch)?;
    if !epoch.bootstrap || marker != epoch.marker_after.as_bytes() {
        return Ok(None);
    }
    if title.is_some_and(|title| note.canonical.as_ref().unwrap().title() != title) {
        return Err(conflict(
            "this initialized wiki has a different title; omit --title to finish setup without renaming it",
        ));
    }
    Ok(Some(epoch.vault_id))
}
#[derive(Clone)]
enum OldTarget {
    Source(RecordId),
    Capture(RecordId, RecordId),
    Original(VaultRelativePath),
}
fn decode_destination(destination: &str) -> Result<String> {
    let mut out = Vec::new();
    let bytes = destination.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let pair = bytes
                .get(at + 1..at + 3)
                .ok_or_else(|| WikiError::invalid("incomplete encoded Markdown link"))?;
            out.push(
                u8::from_str_radix(
                    std::str::from_utf8(pair)
                        .map_err(|_| WikiError::invalid("invalid encoded link"))?,
                    16,
                )
                .map_err(|_| WikiError::invalid("invalid encoded link"))?,
            );
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    utf8(out)
}
fn resolved_target(
    owner: &VaultRelativePath,
    destination: &str,
    syntax: LinkSyntax,
    registry: &[RegistryEntry],
    targets: &BTreeMap<VaultRelativePath, OldTarget>,
) -> Result<Option<(OldTarget, Option<String>)>> {
    if destination.contains("://")
        || destination.starts_with('#')
        || destination.starts_with("mailto:")
    {
        return Ok(None);
    }
    // A percent-encoded # belongs to the filename, not to the fragment syntax.
    let (name, fragment) = destination
        .split_once('#')
        .map_or((destination, None), |(name, fragment)| {
            (name, Some(fragment.to_owned()))
        });
    let decoded = if syntax == LinkSyntax::Markdown {
        decode_destination(name)?
    } else {
        name.to_owned()
    };
    let name = decoded.as_str();
    let mut candidates = Vec::new();
    if syntax == LinkSyntax::Markdown {
        let mut parts: Vec<_> = owner.as_str().split('/').collect();
        parts.pop();
        for part in name.split('/') {
            match part {
                "." => {}
                ".." => {
                    if parts.pop().is_none() {
                        return Ok(None);
                    }
                }
                "" => return Ok(None),
                part => parts.push(part),
            }
        }
        if let Ok(path) = rel(parts.join("/")) {
            candidates.push(path);
        }
    }
    if let Ok(path) = rel(name) {
        candidates.push(path);
    }
    for path in candidates {
        if let Some(target) = targets.get(&path) {
            return Ok(Some((target.clone(), fragment)));
        }
    }
    if syntax == LinkSyntax::Wiki {
        match crate::records::resolve_untyped(registry, destination) {
            LinkResolution::Resolved { path, fragment, .. } => {
                if let Some(target) = targets.get(&path) {
                    return Ok(Some((target.clone(), fragment)));
                }
            }
            LinkResolution::Ambiguous { ids }
                if ids.iter().any(|id| {
                    targets.values().any(|target| match target {
                        OldTarget::Source(source) => source == id,
                        OldTarget::Capture(_, revision) => revision == id,
                        OldTarget::Original(_) => false,
                    })
                }) =>
            {
                return Err(WikiError::invalid(format!(
                    "ambiguous source link in {owner}: {destination}"
                )));
            }
            _ => {}
        }
    }
    Ok(None)
}
fn human_destination(
    owner: &VaultRelativePath,
    path: &VaultRelativePath,
    fragment: Option<&str>,
    syntax: LinkSyntax,
) -> String {
    let mut out = if syntax == LinkSyntax::Wiki {
        path.as_str().to_owned()
    } else {
        let mut parent: Vec<_> = owner.as_str().split('/').collect();
        parent.pop();
        let target: Vec<_> = path.as_str().split('/').collect();
        let shared = parent
            .iter()
            .zip(&target)
            .take_while(|(a, b)| a == b)
            .count();
        let mut parts = vec![".."; parent.len() - shared];
        parts.extend_from_slice(&target[shared..]);
        let mut encoded = String::new();
        for byte in parts.join("/").bytes() {
            if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
                encoded.push(char::from(byte));
            } else {
                encoded.push_str(&format!("%{byte:02X}"));
            }
        }
        if encoded.starts_with("../") {
            encoded
        } else {
            format!("./{encoded}")
        }
    };
    if let Some(fragment) = fragment {
        out.push('#');
        out.push_str(fragment);
    }
    out
}
fn analyze_snapshot(
    vault_id: RecordId,
    kind: LayoutKind,
    files: Vec<StorageFile>,
    limits: &SourceLayoutOptions,
    read: &mut dyn FnMut(&StorageFile) -> Result<Vec<u8>>,
) -> Result<Analysis> {
    // The held writer updates this ephemeral diagnostic on every acquisition.
    // It is neither accounting authority nor a restorable part of the backup.
    let files: Vec<_> = files
        .into_iter()
        .filter(|file| file.path.as_str() != ".wiki/state/writer.lock")
        .collect();
    let inventory: BTreeMap<_, _> = files.iter().map(|file| (file.path.clone(), file)).collect();
    let mut originals = BTreeMap::<VaultRelativePath, Vec<u8>>::new();
    let mut notes = BTreeMap::<VaultRelativePath, ParsedNote>::new();
    let mut copies = Vec::new();
    let mut blockers = Vec::new();
    for file in &files {
        if file.bytes > MAX_FILE as u64 {
            return Err(budget(format!(
                "{} exceeds the existing 64 MiB migration file limit",
                file.path
            )));
        }
        let logical =
            layout::logical_path_for(kind, &file.path).unwrap_or_else(|| file.path.clone());
        if logical
            .as_str()
            .starts_with(crate::domain::source_presentation::PRESENTATION_PREFIX)
        {
            blockers.push(format!(
                "reserved saved-note destination already exists: {}",
                file.path
            ));
        }
        if let Some(destination) = layout::managed_path_for(LayoutKind::ReadableSources, &logical)
            && destination != file.path
        {
            if inventory.contains_key(&destination) {
                blockers.push(format!(
                    "migration destination already exists: {destination}"
                ));
            }
            copies.push(Copy {
                source: file.path.clone(),
                destination,
                hash: file.hash.clone(),
                bytes: file.bytes,
            });
        }
        let canonical_candidate = crate::sources::revision::canonical_path(&logical);
        let parts: Vec<_> = logical.as_str().split('/').collect();
        let extension = logical
            .as_str()
            .rsplit_once('.')
            .map(|(_, extension)| extension);
        let authored_markdown = extension.is_some_and(|extension| {
            extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown")
        }) && !parts.iter().any(|part| part.starts_with('.'))
            && !parts
                .first()
                .is_some_and(|part| matches!(*part, "changes" | "runs"))
            && !logical.as_str().starts_with("knowledge/extractions/")
            && !(parts
                .first()
                .is_some_and(|part| part.eq_ignore_ascii_case("sources"))
                && parts
                    .get(2)
                    .is_some_and(|part| part.eq_ignore_ascii_case("revisions")))
            && layout::managed_path_for(LayoutKind::ReadableSources, &logical).is_none();
        if canonical_candidate || authored_markdown {
            let bytes = read(file)?;
            let mut note = parse_note(&bytes);
            // Migration observes navigation in otherwise excluded Markdown; it
            // must not adopt that file's frontmatter into the canonical registry.
            if !canonical_candidate {
                note.canonical = None;
            }
            notes.insert(logical.clone(), note);
            originals.insert(logical, bytes);
        }
    }
    let mut identities = BTreeSet::new();
    let mut registry = Vec::new();
    let mut sources = BTreeMap::new();
    for (path, note) in &notes {
        if !crate::sources::revision::canonical_path(path) {
            continue;
        }
        if let Some(record) = &note.canonical {
            if !identities.insert(record.id().clone()) {
                blockers.push(format!("duplicate canonical identity {}", record.id()));
            }
            registry.push(RegistryEntry {
                id: record.id().clone(),
                kind: record.kind(),
                path: path.clone(),
                aliases: record
                    .field("aliases")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect(),
            });
            if record.kind() == RecordKind::Source {
                if path.as_str() != format!("sources/{}/source.md", record.id())
                    || layout::source_managed_path(path).is_none()
                {
                    blockers.push(format!(
                        "Source {} uses unsupported custom path {path}; its bytes were preserved",
                        record.id()
                    ));
                } else {
                    sources.insert(record.id().clone(), path.clone());
                }
            }
        } else if note
            .fields
            .as_ref()
            .and_then(|fields| fields.get("wiki_kind"))
            .and_then(serde_json::Value::as_str)
            .is_some_and(|kind| matches!(kind, "source" | "revision"))
        {
            blockers.push(format!("invalid Source or Revision metadata at {path}"));
        }
    }
    let mut targets = BTreeMap::new();
    let mut captures = BTreeMap::<(RecordId, RecordId), (CanonicalRecord, Option<Vec<u8>>)>::new();
    for (id, path) in &sources {
        let source = notes[path].canonical.as_ref().expect("collected Source");
        targets.insert(path.clone(), OldTarget::Source(id.clone()));
        let retained: Vec<_> = source
            .field("wiki_revisions")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .collect();
        if retained.iter().collect::<BTreeSet<_>>().len() != retained.len()
            || !retained.contains(&source.string("wiki_current_revision").unwrap_or_default())
        {
            return Err(WikiError::new(
                ErrorCode::SourceIntegrity,
                format!("Source {id} has invalid retained revision membership"),
            ));
        }
        for revision_id in retained {
            let revision_id = RecordId::new(revision_id)?;
            let revision_path = rel(format!("sources/{id}/revisions/{revision_id}/revision.md"))?;
            let revision = notes
                .get(&revision_path)
                .and_then(|note| note.canonical.as_ref())
                .ok_or_else(|| {
                    WikiError::new(
                        ErrorCode::SourceIntegrity,
                        format!("Source {id} is missing retained revision {revision_id}"),
                    )
                })?;
            if revision.kind() != RecordKind::Revision
                || revision.id() != &revision_id
                || revision.string("wiki_source_id") != Some(id.as_str())
                || revision
                    .string("wiki_source")
                    .is_some_and(|value| value != format!("[[{path}]]"))
            {
                return Err(WikiError::new(
                    ErrorCode::SourceIntegrity,
                    "migration revision ownership or companion differs",
                ));
            }
            if source.string("wiki_current_revision") == Some(revision_id.as_str())
                && source
                    .string("wiki_revision")
                    .is_some_and(|value| value != format!("[[{revision_path}]]"))
            {
                return Err(WikiError::new(
                    ErrorCode::SourceIntegrity,
                    "migration Source current companion differs",
                ));
            }
            targets.insert(
                revision_path.clone(),
                OldTarget::Capture(id.clone(), revision_id.clone()),
            );
            let parent = revision_path
                .as_str()
                .rsplit_once('/')
                .expect("revision parent")
                .0;
            let mut content = None;
            for (path_field, hash_field) in [
                ("wiki_original_path", "wiki_original_hash"),
                ("wiki_content_path", "wiki_content_hash"),
            ] {
                if let Some(name) = revision.string(path_field) {
                    let logical = rel(format!("{parent}/{name}"))?;
                    if layout::source_managed_path(&logical).is_none() {
                        return Err(WikiError::invalid(format!(
                            "unsupported captured payload path {logical}"
                        )));
                    }
                    let physical =
                        layout::managed_path_for(kind, &logical).unwrap_or_else(|| logical.clone());
                    let file = inventory.get(&physical).ok_or_else(|| {
                        WikiError::new(
                            ErrorCode::SourceIntegrity,
                            format!("captured payload {logical} is missing"),
                        )
                    })?;
                    let bytes = read(file)?;
                    if Some(Blake3Hash::digest(&bytes).as_str()) != revision.string(hash_field) {
                        return Err(WikiError::new(
                            ErrorCode::SourceIntegrity,
                            format!("captured payload hash differs at {logical}"),
                        ));
                    }
                    if path_field == "wiki_content_path" {
                        std::str::from_utf8(&bytes).map_err(|_| {
                            WikiError::new(
                                ErrorCode::SourceIntegrity,
                                "capture content is not UTF-8",
                            )
                        })?;
                        content = Some(bytes.clone());
                        targets.insert(
                            logical.clone(),
                            OldTarget::Capture(id.clone(), revision_id.clone()),
                        );
                    } else {
                        targets.insert(logical.clone(), OldTarget::Original(logical.clone()));
                    }
                    originals.insert(logical, bytes);
                }
            }
            captures.insert((id.clone(), revision_id), (revision.clone(), content));
        }
    }
    for (path, note) in &notes {
        if let Some(record) = note
            .canonical
            .as_ref()
            .filter(|record| record.kind() == RecordKind::Revision)
        {
            let owner = RecordId::new(record.string("wiki_source_id").unwrap_or_default())?;
            if !captures.contains_key(&(owner.clone(), record.id().clone()))
                || path.as_str() != format!("sources/{owner}/revisions/{}/revision.md", record.id())
            {
                blockers.push(format!("Revision {} at {path} is outside its Source's fixed retained history; its bytes were preserved", record.id()));
            }
        }
    }
    let authored: Vec<_> = notes
        .iter()
        .filter(|(path, note)| {
            !path.as_str().starts_with('.')
                && (layout::managed_path_for(LayoutKind::ReadableSources, path).is_none()
                    || note
                        .canonical
                        .as_ref()
                        .is_some_and(|record| record.kind() == RecordKind::Source))
        })
        .map(|(path, _)| path.clone())
        .collect();
    let mut requested = BTreeMap::<RecordId, BTreeSet<RecordId>>::new();
    let mut full = BTreeMap::<RecordId, BTreeSet<RecordId>>::new();
    for path in &authored {
        let note = &notes[path];
        let body = if note
            .canonical
            .as_ref()
            .is_some_and(|record| record.kind() == RecordKind::Source)
        {
            note.body()
        } else {
            &note.raw
        };
        for link in
            crate::records::extract_links(std::str::from_utf8(body).map_err(|_| {
                WikiError::invalid(format!("authored Markdown {path} is not UTF-8"))
            })?)
        {
            let target =
                resolved_target(path, &link.destination, link.syntax, &registry, &targets)?;
            if let Some((OldTarget::Original(original), _)) = &target {
                blockers.push(format!("{path} links directly to captured attachment {original}; change that link to its saved source before migration; the attachment bytes were preserved"));
            }
            if let Some((OldTarget::Capture(source, revision), fragment)) = target {
                requested
                    .entry(source.clone())
                    .or_default()
                    .insert(revision.clone());
                if fragment.is_some() {
                    full.entry(source).or_default().insert(revision);
                }
            }
        }
        if note
            .canonical
            .as_ref()
            .is_some_and(|record| record.kind() == RecordKind::Page)
            && let Some(refs) = crate::app::migration_source_refs(&note.raw)?
        {
            for citation in refs.citations() {
                let CitationRef::Source(reference) = citation else {
                    return Err(WikiError::invalid("unsupported migration Page citation"));
                };
                let (_, content) = captures
                    .get(&(
                        reference.source_id.clone(),
                        reference.source_revision.clone(),
                    ))
                    .ok_or_else(|| {
                        WikiError::new(
                            ErrorCode::SourceIntegrity,
                            "Page cites an unretained capture",
                        )
                    })?;
                let content = content.as_ref().ok_or_else(|| {
                    WikiError::new(
                        ErrorCode::SourceIntegrity,
                        "Page cites a capture without text",
                    )
                })?;
                let text = std::str::from_utf8(content).map_err(|_| {
                    WikiError::new(
                        ErrorCode::SourceIntegrity,
                        "Page cites capture content that is not UTF-8",
                    )
                })?;
                if Blake3Hash::digest(reference.span.slice(text)?) != reference.quote_hash {
                    return Err(WikiError::new(
                        ErrorCode::SourceIntegrity,
                        "migration Page quote hash differs",
                    ));
                }
                requested
                    .entry(reference.source_id.clone())
                    .or_default()
                    .insert(reference.source_revision.clone());
            }
        }
    }
    let mut current_paths = BTreeMap::new();
    let mut saved_paths = BTreeMap::new();
    let mut reserved = BTreeSet::new();
    for (id, path) in &sources {
        let source = notes[path].canonical.as_ref().unwrap();
        let current = presentation::allocate_path(source.title(), id, None, &reserved)?;
        reserved.insert(current.to_string());
        current_paths.insert(id.clone(), current);
        for revision in requested.get(id).into_iter().flatten() {
            if source.string("wiki_current_revision") != Some(revision.as_str())
                || full.get(id).is_some_and(|set| set.contains(revision))
            {
                let date = captures[&(id.clone(), revision.clone())]
                    .0
                    .string("wiki_captured_at")
                    .and_then(|date| date.get(..10))
                    .ok_or_else(|| WikiError::invalid("capture date missing"))?;
                let saved =
                    presentation::allocate_path(source.title(), revision, Some(date), &reserved)?;
                reserved.insert(saved.to_string());
                saved_paths.insert((id.clone(), revision.clone()), saved);
            }
        }
    }
    let rewrite = |owner: &VaultRelativePath,
                   displayed_at: &VaultRelativePath,
                   bytes: &[u8]|
     -> Result<Vec<u8>> {
        crate::records::link_rewrite::rewrite_link_destinations(
            std::str::from_utf8(bytes)
                .map_err(|_| WikiError::invalid("authored Markdown is not UTF-8"))?,
            &mut |destination, syntax| {
                let Some((target, fragment)) =
                    resolved_target(owner, destination, syntax, &registry, &targets)?
                else {
                    return Ok(None);
                };
                let (path, fragment) = match target {
                    OldTarget::Original(_) => return Ok(None),
                    OldTarget::Source(source) => (current_paths[&source].clone(), fragment),
                    OldTarget::Capture(source, revision) => {
                        if let Some(fragment) = fragment {
                            (
                                saved_paths
                                    .get(&(source, revision))
                                    .ok_or_else(|| {
                                        WikiError::invalid(
                                            "heading link lacks a demanded full copy",
                                        )
                                    })?
                                    .clone(),
                                Some(fragment),
                            )
                        } else {
                            (
                                current_paths[&source].clone(),
                                Some(format!("^{}", presentation::capture_anchor(&revision))),
                            )
                        }
                    }
                };
                Ok(Some(human_destination(
                    displayed_at,
                    &path,
                    fragment.as_deref(),
                    syntax,
                )))
            },
        )
    };
    let mut replacements = Vec::new();
    let mut navigation = BTreeMap::<(RecordId, RecordId), SourceNavigation>::new();
    let mut rendered_bytes = 0u64;
    for (id, path) in &sources {
        let note = &notes[path];
        let source = note.canonical.as_ref().unwrap();
        let body = rewrite(path, &current_paths[id], note.body())?;
        let planned = presentation_planning::plan_change_for_navigation(
            source,
            &body,
            None,
            &[],
            requested.get(id).unwrap_or(&BTreeSet::new()),
            presentation_planning::NavigationDemand {
                full_copies: full.get(id).unwrap_or(&BTreeSet::new()),
                allocated_current: Some(&current_paths[id]),
                allocated_saved: &saved_paths
                    .iter()
                    .filter(|((owner, _), _)| owner == id)
                    .map(|((_, revision), path)| (revision.clone(), path.clone()))
                    .collect(),
            },
            &mut |path, maximum| {
                let bytes = originals.get(path).cloned();
                if bytes.as_ref().is_some_and(|bytes| bytes.len() > maximum) {
                    return Err(budget("migration presentation input exceeds bound"));
                }
                Ok(bytes)
            },
        )?;
        for (revision, target) in planned.plan.navigation {
            navigation.insert((id.clone(), revision), target);
        }
        let source_after = edit_note(
            note,
            &BTreeMap::from([(
                "wiki_presentation".into(),
                planned.plan.metadata.encode()?.into(),
            )]),
            Some(&body),
            &note.source_hash,
        )?;
        replacements.push(Replacement {
            path: layout::managed_path_for(LayoutKind::ReadableSources, path)
                .ok_or_else(|| WikiError::invalid("Source mapping missing"))?,
            before: Some(utf8(note.raw.clone())?),
            after: utf8(source_after)?,
        });
        for write in planned.plan.writes {
            let bytes = write
                .proposed
                .ok_or_else(|| WikiError::invalid("display cannot be deleted"))?;
            rendered_bytes = rendered_bytes
                .checked_add(bytes.len() as u64)
                .ok_or_else(|| budget("rendered size overflow"))?;
            replacements.push(Replacement {
                path: write.target,
                before: None,
                after: utf8(bytes)?,
            });
        }
    }
    let marker_path = rel("WIKI.md")?;
    let marker_original = utf8(
        originals
            .get(&marker_path)
            .ok_or_else(|| WikiError::invalid("migration marker missing"))?
            .clone(),
    )?;
    let mut marker_after = marker_original.clone();
    let mut authored_link_edits = 0;
    for path in authored {
        let note = &notes[&path];
        if note
            .canonical
            .as_ref()
            .is_some_and(|record| record.kind() == RecordKind::Source)
        {
            continue;
        }
        let mut after = rewrite(&path, &path, &note.raw)?;
        if note
            .canonical
            .as_ref()
            .is_some_and(|record| record.kind() == RecordKind::Page)
        {
            after = crate::app::migrate_source_citation_block(&path, &after, &mut |reference| {
                let source = notes[&sources[&reference.source_id]]
                    .canonical
                    .as_ref()
                    .unwrap();
                let revision = &captures[&(
                    reference.source_id.clone(),
                    reference.source_revision.clone(),
                )]
                    .0;
                let state = if source.string("wiki_status") == Some("withdrawn") {
                    CitationState::Withdrawn
                } else if source.string("wiki_current_revision")
                    == Some(reference.source_revision.as_str())
                {
                    CitationState::Current
                } else {
                    CitationState::Historical
                };
                Ok((
                    source.title().into(),
                    revision.string("wiki_captured_at").unwrap().into(),
                    navigation
                        .get(&(
                            reference.source_id.clone(),
                            reference.source_revision.clone(),
                        ))
                        .ok_or_else(|| WikiError::invalid("citation navigation missing"))?
                        .clone(),
                    state,
                ))
            })?;
        }
        if after != note.raw {
            authored_link_edits += 1;
        }
        if path == marker_path {
            let rewritten = parse_note(&after);
            marker_after = utf8(crate::records::edit::migrate_schema(
                &rewritten,
                "3",
                &rewritten.source_hash,
            )?)?;
        } else if after != note.raw {
            replacements.push(Replacement {
                path,
                before: Some(utf8(note.raw.clone())?),
                after: utf8(after)?,
            });
        }
    }
    copies.sort_by(|a, b| a.destination.cmp(&b.destination));
    replacements.sort_by(|a, b| a.path.cmp(&b.path));
    let sum = |values: Vec<u64>| {
        values.into_iter().try_fold(0u64, |total, size| {
            total
                .checked_add(size)
                .ok_or_else(|| budget("migration byte count overflow"))
        })
    };
    let planned_copy_bytes = sum(copies.iter().map(|item| item.bytes).collect())?;
    let retained_backup_bytes = sum(files.iter().map(|file| file.bytes).collect())?;
    let required_free_bytes = sum(vec![
        planned_copy_bytes,
        retained_backup_bytes,
        sum(replacements
            .iter()
            .map(|item| item.after.len() as u64)
            .collect())?,
        marker_after.len() as u64,
        2 * MAX_RECEIPT as u64,
    ])?;
    if required_free_bytes
        .checked_add(retained_backup_bytes)
        .is_none_or(|total| total > limits.max_bytes)
    {
        blockers.push("inputs, complete backup and output exceed --max-bytes; raise the bound before activation".into());
    }
    if files
        .len()
        .saturating_mul(2)
        .saturating_add(copies.len())
        .saturating_add(replacements.len())
        .saturating_add(4)
        > limits.max_files
    {
        blockers.push(
            "inputs, backup and output exceed --max-files; raise the bound before activation"
                .into(),
        );
    }
    let plan_hash = Blake3Hash::digest(
        serde_json::to_vec(&(&files, &copies, &replacements, &marker_after))
            .map_err(|e| WikiError::invalid(e.to_string()))?,
    );
    let plan = SourceLayoutPlan { vault_id, plan_hash, from_layout: kind as u32, to_layout: 3, complete: blockers.is_empty(), sources: sources.len(), historical_copies: saved_paths.len(), authored_link_edits, planned_copy_bytes, retained_backup_bytes, rendered_bytes, required_free_bytes, blockers, warnings: vec!["Every existing undo payload is retained. Previously expired payloads cannot be restored by migration. The backup excludes .git and ephemeral writer-lock diagnostics, which remain in place. The space estimate reserves 128 MiB for bounded recovery receipts. To restore, copy the backup vault folder contents into a separate folder, then copy marker/WIKI.before there as WIKI.md.".into()] };
    Ok(Analysis {
        marker_original,
        marker_after,
        files,
        copies,
        replacements,
        plan,
    })
}
pub fn plan_source_layout(fs: &VaultFs, limits: &SourceLayoutOptions) -> Result<SourceLayoutPlan> {
    super::inventory::validate(&options(limits))?;
    if let Some(bytes) = layout::raw_read(fs.root(), &rel(PENDING)?, MAX_RECEIPT)? {
        let epoch: Epoch = layout::decode(&bytes)?;
        validate_epoch(&epoch)?;
        return Ok(epoch.plan);
    }
    let kind = layout::layout_kind(fs.root())?;
    let inventory = super::inventory::inventory(fs, &options(limits))?;
    if !inventory.complete {
        return Err(budget(format!(
            "migration inventory is incomplete: {}",
            inventory.warnings.join("; ")
        )));
    }
    if kind == LayoutKind::ReadableSources {
        let mut plan = audit_presentations(fs, &inventory)?;
        plan.warnings
            .push("This wiki already uses readable saved notes.".into());
        return Ok(plan);
    }
    let analysis = analyze_snapshot(
        inventory.vault_id,
        kind,
        inventory.files,
        limits,
        &mut |file| read_exact(fs.root(), file),
    )?;
    let changed: BTreeSet<_> = analysis
        .replacements
        .iter()
        .map(|item| {
            layout::logical_path_for(LayoutKind::ReadableSources, &item.path)
                .unwrap_or_else(|| item.path.clone())
        })
        .chain(std::iter::once(rel("WIKI.md")?))
        .collect();
    let mut plan = analysis.plan;
    let engine = ChangeEngine::new(fs.clone())?;
    for id in engine.change_ids()? {
        if !matches!(
            engine.inspect_history(&id)?.status,
            ChangeStatus::Committed | ChangeStatus::Aborted
        ) {
            plan.blockers.push(format!(
                "apply or abandon pending change {id} before migration"
            ));
        }
    }
    for run in RunStore::discover_existing(fs, &plan.vault_id, 4096, &mut || Ok(()))? {
        let history = crate::jobs::checkpoint::storage_run_history(fs, &run)?;
        let state = crate::jobs::replay_storage_frames(&history.frames)?;
        if !matches!(
            state.state,
            crate::jobs::RunState::Completed | crate::jobs::RunState::Failed
        ) {
            for guard in &state.spec.scope.read_preconditions {
                if changed.contains(&guard.path) {
                    plan.blockers.push(format!("active job {run} guards {}; finish or explicitly reconcile that job before migration; its charge holds were preserved", guard.path));
                }
            }
        }
    }
    plan.complete &= plan.blockers.is_empty();
    Ok(plan)
}
fn audit_presentations(fs: &VaultFs, inventory: &StorageInventory) -> Result<SourceLayoutPlan> {
    let mut plan = empty_plan(inventory.vault_id.clone(), 3);
    let mut owned = BTreeMap::new();
    for file in &inventory.files {
        let Some(logical) = layout::logical_path_for(LayoutKind::ReadableSources, &file.path)
        else {
            continue;
        };
        if !logical.as_str().ends_with("/source.md") {
            continue;
        }
        let note = parse_note(&read_exact(fs.root(), file)?);
        let record = note
            .canonical
            .as_ref()
            .filter(|record| record.kind() == RecordKind::Source)
            .ok_or_else(|| recovery(format!("invalid hidden Source at {logical}")))?;
        if logical.as_str() != format!("sources/{}/source.md", record.id()) {
            return Err(recovery("hidden Source identity differs from fixed path"));
        }
        let presentation =
            crate::domain::source_presentation::SourcePresentation::from_record(record)?
                .ok_or_else(|| {
                    recovery(format!(
                        "Source {} lacks saved-note ownership metadata",
                        record.id()
                    ))
                })?;
        plan.sources += 1;
        let entries = std::iter::once((
            &presentation.current.path,
            &presentation.current.generated_hash,
        ))
        .chain(
            presentation
                .published
                .values()
                .flatten()
                .map(|saved| (&saved.path, &saved.generated_hash)),
        );
        for (path, hash) in entries {
            if owned.insert(path.clone(), hash.clone()).is_some() {
                return Err(recovery(format!(
                    "saved note {path} has more than one owner"
                )));
            }
        }
        plan.historical_copies += presentation.published.values().flatten().count();
    }
    for file in &inventory.files {
        if file
            .path
            .as_str()
            .starts_with(crate::domain::source_presentation::PRESENTATION_PREFIX)
        {
            match owned.remove(&file.path) {
                Some(hash) if hash == file.hash => {},
                Some(_) => plan.blockers.push(format!("saved note {} was edited; preserve those edits in the original note or a Page before explicitly restoring its generated copy", file.path)),
                None => plan.blockers.push(format!("unowned file {} occupies the saved-note folder; move it to an author folder before continuing", file.path)),
            }
        }
    }
    plan.blockers.extend(
        owned
            .keys()
            .map(|path| format!("owned saved note is missing: {path}")),
    );
    plan.complete = plan.blockers.is_empty();
    let stable_files: Vec<_> = inventory
        .files
        .iter()
        .filter(|file| file.path.as_str() != ".wiki/state/writer.lock")
        .collect();
    plan.plan_hash =
        Blake3Hash::digest(serde_json::to_vec(&stable_files).map_err(|e| recovery(e.to_string()))?);
    Ok(plan)
}
fn replace(
    fs: &VaultFs,
    writer: &WriterPermit,
    path: &VaultRelativePath,
    before: Option<&[u8]>,
    after: &[u8],
) -> Result<()> {
    let current = layout::raw_read(fs.root(), path, MAX_FILE)?;
    if current.as_deref() == Some(after) {
        return crate::changes::journal::require_sync(fs.sync_target(path, writer)?);
    }
    if current.as_deref() != before {
        return Err(conflict(format!(
            "migration preserved unexpected bytes at {path}"
        )));
    }
    if let Some((parent, _)) = path.as_str().rsplit_once('/') {
        fs.ensure_directory(&rel(parent)?, writer)?;
    }
    let expected = before.map_or(ExpectedState::Absent, |bytes| {
        ExpectedState::Hash(Blake3Hash::digest(bytes))
    });
    let staged = fs.stage(path, after, writer)?;
    crate::changes::journal::require_sync(fs.replace(staged, &expected, writer)?)
}
fn remove_exact(
    fs: &VaultFs,
    path: &VaultRelativePath,
    hash: &Blake3Hash,
    length: u64,
) -> Result<()> {
    let Some(bytes) = layout::raw_read(fs.root(), path, MAX_FILE)? else {
        return Ok(());
    };
    if bytes.len() as u64 != length || Blake3Hash::digest(&bytes) != *hash {
        return Err(conflict(format!("migration preserved changed file {path}")));
    }
    let actual = fs.root().resolve_raw(path)?;
    let io = fs.durable_io();
    io.remove(&actual).map_err(|e| recovery(e.to_string()))?;
    crate::changes::journal::require_sync(
        io.sync_directory(
            actual
                .parent()
                .ok_or_else(|| recovery("unlink parent missing"))?,
        )
        .map_err(|e| recovery(e.to_string()))?,
    )
}
#[cfg(unix)]
// libc's statvfs field widths vary across Unix targets. Widen both operands
// before multiplication so 32-bit targets retain the u64 disk-space calculation.
#[allow(clippy::unnecessary_cast)]
fn available_bytes(root: &VaultRoot) -> Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(root.path().as_os_str().as_bytes())
        .map_err(|_| WikiError::invalid("wiki path contains NUL"))?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: path is NUL-terminated and stats points to writable storage.
    if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return Err(recovery(format!(
            "cannot check migration disk space: {}",
            std::io::Error::last_os_error()
        )));
    }
    // SAFETY: successful statvfs initialized the structure.
    let stats = unsafe { stats.assume_init() };
    Ok((stats.f_bavail as u64).saturating_mul(stats.f_frsize as u64))
}
#[cfg(windows)]
fn available_bytes(root: &VaultRoot) -> Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    let path: Vec<_> = root
        .path()
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let mut available = 0;
    // SAFETY: path is terminated and available is a valid output pointer.
    if unsafe {
        windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
            path.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(recovery("cannot check migration disk space"));
    }
    Ok(available)
}
#[cfg(not(any(unix, windows)))]
fn available_bytes(_: &VaultRoot) -> Result<u64> {
    Err(WikiError::new(
        ErrorCode::CapabilityUnavailable,
        "migration disk-space check unavailable",
    ))
}
fn resume(
    fs: &VaultFs,
    writer: &WriterPermit,
    epoch: Epoch,
    resumed: bool,
    limits: &SourceLayoutOptions,
) -> Result<SourceLayoutReport> {
    let expected = layout::encode(&Layout {
        version: 3,
        vault_id: epoch.vault_id.clone(),
        migration_id: epoch.id.clone(),
        migration_hash: Blake3Hash::digest(layout::encode(&epoch)?),
    })?;
    let id = epoch.id.clone();
    resume_inner(fs, writer, epoch, resumed, limits).map_err(|mut error| {
        match layout::raw_read(fs.root(), &rel(layout::ACTIVE).expect("fixed path"), 4096) {
            Ok(Some(bytes)) if bytes == expected => {
                error.details = serde_json::json!({"migration_committed":true,"operation_id":id,"cause":error.details});
                error.hint = Some("The saved-note layout is committed. Run `lwiki storage migrate` again to finish recovery and index publication.".into());
            },
            Err(observation) => { error.details = serde_json::json!({"operation_id":id,"cause":error.details,"activation_check_error":observation.message}); },
            _ => { error.hint = Some("Migration is incomplete and its original files are retained. Run `lwiki storage migrate` again to resume the exact saved plan.".into()); },
        }
        error
    })
}
fn resume_inner(
    fs: &VaultFs,
    writer: &WriterPermit,
    epoch: Epoch,
    resumed: bool,
    limits: &SourceLayoutOptions,
) -> Result<SourceLayoutReport> {
    validate_epoch(&epoch)?;
    if layout::vault(fs.root())?.0 != epoch.vault_id {
        return Err(recovery("migration belongs to another wiki"));
    }
    let rebuilt = analyze_snapshot(
        epoch.vault_id.clone(),
        LayoutKind::from_version(epoch.from_layout)?,
        epoch.files.clone(),
        limits,
        &mut |file| {
            let backup = backup_path(&epoch.id, &file.path)?;
            if let Some(bytes) = layout::raw_read(fs.root(), &backup, MAX_FILE)? {
                if bytes.len() as u64 != file.bytes || Blake3Hash::digest(&bytes) != file.hash {
                    return Err(recovery(format!("backup changed at {backup}")));
                }
                Ok(bytes)
            } else {
                read_exact(fs.root(), file)
            }
        },
    )?;
    if rebuilt.plan.plan_hash != epoch.plan.plan_hash
        || rebuilt.copies != epoch.copies
        || rebuilt.replacements != epoch.replacements
        || rebuilt.marker_after != epoch.marker_after
        || !rebuilt.plan.complete
    {
        return Err(recovery(
            "migration actions differ from exact preserved-input replay",
        ));
    }
    let targets: BTreeSet<_> = epoch
        .copies
        .iter()
        .map(|copy| Ok(copy.destination.clone()))
        .chain(epoch.replacements.iter().map(|item| Ok(item.path.clone())))
        .chain(
            epoch
                .files
                .iter()
                .map(|file| backup_path(&epoch.id, &file.path)),
        )
        .collect::<Result<_>>()?;
    fs.root()
        .validate_portable_paths(&targets.into_iter().collect::<Vec<_>>())?;
    let encoded = layout::encode(&epoch)?;
    if encoded.len() > MAX_RECEIPT {
        return Err(budget("migration receipt exceeds 64 MiB"));
    }
    layout::put(fs, writer, &archive(&epoch.id)?, &encoded)?;
    checkpoint("receipt", fs)?;
    for file in &epoch.files {
        let destination = backup_path(&epoch.id, &file.path)?;
        if let Some(bytes) = layout::raw_read(fs.root(), &destination, MAX_FILE)? {
            if bytes.len() as u64 != file.bytes || Blake3Hash::digest(&bytes) != file.hash {
                return Err(recovery("migration backup differs"));
            }
        } else {
            layout::put(fs, writer, &destination, &read_exact(fs.root(), file)?)?;
        }
    }
    for copy in &epoch.copies {
        if let Some(bytes) = layout::raw_read(fs.root(), &copy.destination, MAX_FILE)? {
            if bytes.len() as u64 == copy.bytes && Blake3Hash::digest(&bytes) == copy.hash
                || epoch
                    .replacements
                    .iter()
                    .any(|item| item.path == copy.destination && item.after.as_bytes() == bytes)
            {
                continue;
            }
            return Err(conflict(format!(
                "migration destination has unfamiliar bytes: {}",
                copy.destination
            )));
        }
        let bytes = layout::raw_read(fs.root(), &backup_path(&epoch.id, &copy.source)?, MAX_FILE)?
            .ok_or_else(|| recovery("copy backup missing"))?;
        if bytes.len() as u64 != copy.bytes || Blake3Hash::digest(&bytes) != copy.hash {
            return Err(recovery("copy backup hash differs"));
        }
        layout::put(fs, writer, &copy.destination, &bytes)?;
    }
    checkpoint("copies", fs)?;
    for item in &epoch.replacements {
        replace(
            fs,
            writer,
            &item.path,
            item.before.as_deref().map(str::as_bytes),
            item.after.as_bytes(),
        )?;
    }
    checkpoint("replacements", fs)?;
    let active = layout::encode(&Layout {
        version: 3,
        vault_id: epoch.vault_id.clone(),
        migration_id: epoch.id.clone(),
        migration_hash: Blake3Hash::digest(&encoded),
    })?;
    if layout::raw_read(fs.root(), &rel(layout::ACTIVE)?, 4096)?.as_ref() != Some(&active) {
        recheck_before_activation(fs, &epoch, &encoded, limits)?;
    }
    replace(
        fs,
        writer,
        &rel("WIKI.md")?,
        Some(epoch.marker_original.as_bytes()),
        epoch.marker_after.as_bytes(),
    )?;
    checkpoint("marker", fs)?;
    replace(
        fs,
        writer,
        &rel(layout::ACTIVE)?,
        epoch.old_activation.as_deref(),
        &active,
    )?;
    checkpoint("activation", fs)?;
    let engine = ChangeEngine::new(fs.clone())?;
    for id in engine.change_ids()? {
        engine.inspect_history(&id)?;
    }
    for run in RunStore::discover_existing(fs, &epoch.vault_id, 4096, &mut || Ok(()))? {
        crate::jobs::checkpoint::storage_run_history(fs, &run)?;
    }
    for copy in &epoch.copies {
        remove_exact(fs, &copy.source, &copy.hash, copy.bytes)?;
    }
    remove_empty_legacy_directories(fs, &epoch)?;
    checkpoint("unlinked", fs)?;
    let report = SourceLayoutReport {
        operation_id: epoch.id.clone(),
        resumed,
        layout_version: 3,
        sources: epoch.plan.sources,
        historical_copies: epoch.plan.historical_copies,
        authored_link_edits: epoch.plan.authored_link_edits,
        copied_bytes: epoch.plan.planned_copy_bytes,
        rendered_bytes: epoch.plan.rendered_bytes,
        backup: Some(backup_root(&epoch.id)),
        warnings: epoch.plan.warnings.clone(),
    };
    if let Err(mut error) = Catalog::new(fs.clone(), epoch.vault_id.clone()).sync_normalized(writer)
    {
        error.details = serde_json::json!({"migration_committed":true,"operation_id":epoch.id,"cause":error.details});
        error.hint = Some("The saved-note layout is committed. Run `lwiki storage migrate` again to finish rebuilding its index from the retained migration.".into());
        return Err(error);
    }
    checkpoint("published", fs)?;
    let mut stored_report = report.clone();
    stored_report.resumed = false;
    layout::put(
        fs,
        writer,
        &rel(format!(".wiki/state/storage/receipts/{}.json", epoch.id))?,
        &layout::encode(&stored_report)?,
    )?;
    checkpoint("completion", fs)?;
    remove_exact(
        fs,
        &rel(PENDING)?,
        &Blake3Hash::digest(&encoded),
        encoded.len() as u64,
    )?;
    Ok(report)
}
fn recheck_before_activation(
    fs: &VaultFs,
    epoch: &Epoch,
    encoded: &[u8],
    limits: &SourceLayoutOptions,
) -> Result<()> {
    let inventory = super::inventory::inventory(fs, &options(limits))?;
    if !inventory.complete {
        return Err(budget(
            "migration cannot activate an incomplete final inventory",
        ));
    }
    let mut expected: BTreeMap<VaultRelativePath, (Blake3Hash, u64)> = epoch
        .files
        .iter()
        .map(|file| (file.path.clone(), (file.hash.clone(), file.bytes)))
        .collect();
    for file in &epoch.files {
        expected.insert(
            backup_path(&epoch.id, &file.path)?,
            (file.hash.clone(), file.bytes),
        );
    }
    for copy in &epoch.copies {
        expected.insert(copy.destination.clone(), (copy.hash.clone(), copy.bytes));
    }
    for item in &epoch.replacements {
        expected.insert(
            item.path.clone(),
            (
                Blake3Hash::digest(item.after.as_bytes()),
                item.after.len() as u64,
            ),
        );
    }
    for path in [rel(PENDING)?, archive(&epoch.id)?] {
        expected.insert(path, (Blake3Hash::digest(encoded), encoded.len() as u64));
    }
    // A prior attempt may have saved the marker but not activation.
    let marker = layout::raw_read(fs.root(), &rel("WIKI.md")?, MAX_FILE)?
        .ok_or_else(|| conflict("migration marker disappeared"))?;
    if marker != epoch.marker_original.as_bytes() && marker != epoch.marker_after.as_bytes() {
        return Err(conflict("migration marker changed"));
    }
    expected.insert(
        rel("WIKI.md")?,
        (Blake3Hash::digest(&marker), marker.len() as u64),
    );
    for file in inventory.files {
        if file.path.as_str() == ".wiki/state/writer.lock" {
            continue;
        }
        if expected.remove(&file.path) != Some((file.hash, file.bytes)) {
            return Err(conflict(format!(
                "migration inventory changed at {}; no layout was activated and all bytes were preserved",
                file.path
            )));
        }
    }
    if let Some(path) = expected.keys().next() {
        return Err(conflict(format!(
            "migration input or output disappeared: {path}"
        )));
    }
    Ok(())
}
fn remove_empty_legacy_directories(fs: &VaultFs, epoch: &Epoch) -> Result<()> {
    let mut directories = BTreeSet::new();
    for copy in &epoch.copies {
        let mut parent = copy
            .source
            .as_str()
            .rsplit_once('/')
            .map(|(parent, _)| parent);
        while let Some(value) = parent {
            let path = rel(value)?;
            if value == "sources"
                || layout::managed_path_for(LayoutKind::ReadableSources, &path).is_none()
            {
                break;
            }
            directories.insert(path);
            parent = value.rsplit_once('/').map(|(parent, _)| parent);
        }
    }
    for directory in directories.iter().rev() {
        let path = fs.root().resolve_raw(directory)?;
        match std::fs::read_dir(&path) {
            Ok(mut entries) => {
                if entries
                    .next()
                    .transpose()
                    .map_err(|e| recovery(e.to_string()))?
                    .is_none()
                {
                    let io = fs.durable_io();
                    io.remove_directory(&path)
                        .map_err(|e| recovery(e.to_string()))?;
                    crate::changes::journal::require_sync(
                        io.sync_directory(
                            path.parent()
                                .ok_or_else(|| recovery("directory parent missing"))?,
                        )
                        .map_err(|e| recovery(e.to_string()))?,
                    )?;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(recovery(error.to_string())),
        }
    }
    Ok(())
}
#[cfg(test)]
type Checkpoint = Box<dyn Fn(&str, &VaultFs) -> Result<()>>;
#[cfg(test)]
thread_local! { static CHECKPOINT: std::cell::RefCell<Option<Checkpoint>> = const { std::cell::RefCell::new(None) }; }
#[cfg(test)]
fn with_checkpoint<T>(hook: Checkpoint, operation: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            CHECKPOINT.with(|slot| *slot.borrow_mut() = None);
        }
    }
    CHECKPOINT.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some(hook);
    });
    let _reset = Reset;
    operation()
}
fn checkpoint(name: &str, fs: &VaultFs) -> Result<()> {
    #[cfg(test)]
    CHECKPOINT.with(|slot| slot.borrow().as_ref().map_or(Ok(()), |hook| hook(name, fs)))?;
    let _ = (name, fs);
    Ok(())
}
pub fn migrate_source_layout(
    fs: &VaultFs,
    writer: &WriterPermit,
    limits: &SourceLayoutOptions,
) -> Result<SourceLayoutReport> {
    super::inventory::validate(&options(limits))?;
    writer.require_root(fs.root())?;
    let fs = fs.for_storage_recovery();
    let (vault_id, _) = layout::vault(fs.root())?;
    let stores = RunStore::discover_existing(&fs, &vault_id, 4096, &mut || Ok(()))?
        .iter()
        .map(|id| RunStore::open_existing(&fs, &vault_id, id))
        .collect::<Result<Vec<_>>>()?;
    let _guards = stores
        .iter()
        .map(|store| store.lock(Duration::from_secs(5), &mut || Ok(())))
        .collect::<Result<Vec<_>>>()?;
    if layout::raw_read(
        fs.root(),
        &rel(".wiki/state/storage/cleanup.json")?,
        MAX_RECEIPT,
    )?
    .is_some()
    {
        return Err(recovery(
            "finish interrupted storage cleanup before migration",
        ));
    }
    if let Some(bytes) = layout::raw_read(fs.root(), &rel(PENDING)?, MAX_RECEIPT)? {
        let epoch: Epoch = layout::decode(&bytes)?;
        if limits
            .expected_plan
            .as_ref()
            .is_some_and(|hash| hash != &epoch.plan.plan_hash)
        {
            return Err(conflict("pending migration differs from expected plan"));
        }
        return resume(&fs, writer, epoch, true, limits);
    }
    let public = plan_source_layout(&fs, limits)?;
    if limits
        .expected_plan
        .as_ref()
        .is_some_and(|hash| hash != &public.plan_hash)
    {
        return Err(conflict("migration plan changed; preview again"));
    }
    if !public.complete || !public.blockers.is_empty() {
        return Err(recovery(public.blockers.join("; ")));
    }
    if public.from_layout == 3 {
        return Ok(SourceLayoutReport {
            operation_id: RecordId::new("source_layout_noop")?,
            resumed: false,
            layout_version: 3,
            sources: public.sources,
            historical_copies: public.historical_copies,
            authored_link_edits: 0,
            copied_bytes: 0,
            rendered_bytes: 0,
            backup: None,
            warnings: public.warnings,
        });
    }
    let inventory = super::inventory::inventory(&fs, &options(limits))?;
    if !inventory.complete {
        return Err(budget("migration inventory became incomplete"));
    }
    let analysis = analyze_snapshot(
        vault_id.clone(),
        LayoutKind::from_version(public.from_layout)?,
        inventory.files,
        limits,
        &mut |file| read_exact(fs.root(), file),
    )?;
    if analysis.plan.plan_hash != public.plan_hash {
        return Err(conflict("migration inputs changed after preview"));
    }
    if available_bytes(fs.root())? < analysis.plan.required_free_bytes {
        return Err(budget(
            "not enough free disk space for complete backup and migration output",
        ));
    }
    let epoch = Epoch {
        version: 1,
        bootstrap: false,
        retention: Retention::PreserveAll,
        id: RecordId::new(format!("source_layout_{}", uuid::Uuid::now_v7()))?,
        vault_id,
        from_layout: public.from_layout,
        marker_original: analysis.marker_original,
        marker_after: analysis.marker_after,
        old_activation: layout::raw_read(fs.root(), &rel(layout::ACTIVE)?, 4096)?,
        files: analysis.files,
        copies: analysis.copies,
        replacements: analysis.replacements,
        plan: analysis.plan,
    };
    validate_epoch(&epoch)?;
    let encoded = layout::encode(&epoch)?;
    if encoded.len() > MAX_RECEIPT {
        return Err(budget(
            "migration receipt exceeds 64 MiB; no layout was activated",
        ));
    }
    layout::put(&fs, writer, &rel(PENDING)?, &encoded)?;
    resume(&fs, writer, epoch, false, limits)
}

#[cfg(test)]
#[path = "source_layout_tests.rs"]
mod tests;

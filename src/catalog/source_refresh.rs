//! Retained bounded source-update publication, sharing the canonical change engine.
//!
//! SQL actions are retained before canonical mutation. Recovery recognizes the
//! exact intended header before trying to reopen a superseded starting snapshot.
use super::{
    Catalog, PublicationCheckpoint,
    normalized_delta::CatalogDelta,
    normalized_read,
    query::QuerySnapshot,
    query_types::{QueryCatalog, QueryReadLimits},
    scan, selector, sql,
};
use crate::{
    changes::{
        ChangeEngine, ChangeStatus, PreparedChange, ReadDependency, RevisionOwnerRow,
        RevisionOwnershipLookup,
        indexed_refresh::{IndexedRefreshPhase, IndexedRefreshProof, IndexedWriteOperation},
        journal,
        operation_authority::{self, ActiveOperation, Authority, Presence, Publication},
        prepare::{MAX_PAYLOAD_BYTES, read_bounded, strict_json},
    },
    domain::{
        Blake3Hash, ErrorCode, ReadSnapshot, RecordId, RecordKind, Result, VaultRelativePath,
        WikiError,
    },
    vault::{ExpectedState, VaultFs, VaultRoot, WriterPermit},
};
use rusqlite::{Transaction, TransactionBehavior, limits::Limit, params};
use serde::{Deserialize, Serialize};
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

const DELTA_VERSION: u32 = 3;
const MAX_DELTA_BYTES: usize = 256 * 1024 * 1024;
const MAX_SELECTED_BYTES: usize = 256 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetainedDelta {
    version: u32,
    vault_id: RecordId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_id: Option<RecordId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    operation: Option<IndexedWriteOperation>,
    change: PreparedChange,
    base: ReadSnapshot,
    before: Vec<ReadDependency>,
    after: Vec<ReadDependency>,
    rows: CatalogDelta,
}

/// A frozen import intent compares trusted preparation results; it cannot
/// authorize canonical mutation or replace the normal retained proof.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NamedIndexedIntent {
    pub manifest: crate::changes::ChangeManifest,
    pub proof: IndexedRefreshProof,
}

pub(crate) struct SealedIndexedPreparation {
    engine: ChangeEngine,
    change: crate::changes::prepare::NamedPreparation,
    delta: RetainedDelta,
    intent: NamedIndexedIntent,
}

impl SealedIndexedPreparation {
    pub(crate) fn intent(&self) -> &NamedIndexedIntent {
        &self.intent
    }
}

/// Narrow path authority from semantic admission or its exact retained replay.
/// Existing prefixes are frozen from before-images, never inferred from live
/// existence: partially applied new allocations still need collision discovery.
#[derive(Clone)]
pub(crate) struct PublishedRefreshPaths {
    root: VaultRoot,
    allowed: BTreeSet<String>,
    published: BTreeSet<String>,
    singleton_sources: BTreeSet<String>,
}
impl PublishedRefreshPaths {
    fn admitted(
        root: &VaultRoot,
        before: &[ReadDependency],
        after: &[ReadDependency],
        operation: Option<&IndexedWriteOperation>,
    ) -> Self {
        fn prefixes(path: &VaultRelativePath) -> impl Iterator<Item = String> + '_ {
            path.as_str()
                .match_indices('/')
                .map(|(end, _)| path.as_str()[..end].to_owned())
                .chain(std::iter::once(path.as_str().to_owned()))
        }
        Self {
            root: root.clone(),
            singleton_sources: operation
                .into_iter()
                .flat_map(IndexedWriteOperation::capture_targets)
                .filter(|capture| {
                    capture.source_id.as_str().len() == 39
                        && capture
                            .source_id
                            .as_str()
                            .bytes()
                            .all(|byte| byte.is_ascii_digit())
                })
                .map(|capture| format!("sources/{}", capture.source_id))
                .collect(),
            allowed: before
                .iter()
                .chain(after)
                .flat_map(|dep| prefixes(&dep.path))
                .collect(),
            published: before
                .iter()
                .filter(|dep| matches!(dep.expected, ExpectedState::Hash(_)))
                .flat_map(|dep| prefixes(&dep.path))
                .collect(),
        }
    }
    pub(crate) fn require_root(&self, root: &VaultRoot) -> Result<()> {
        if &self.root != root {
            return Err(recovery("published refresh paths belong to another vault"));
        }
        Ok(())
    }
    pub(crate) fn allows(&self, path: &VaultRelativePath) -> bool {
        self.allowed.contains(path.as_str()) && crate::storage::layout::managed_path(path).is_none()
    }
    pub(crate) fn published(&self, prefix: &str) -> bool {
        self.published.contains(prefix)
    }
    pub(crate) fn singleton_source(&self, prefix: &str) -> bool {
        self.singleton_sources.contains(prefix)
    }
}

pub(crate) struct IndexedRefreshSession<'a> {
    catalog: Catalog,
    writer: &'a WriterPermit,
    sql_writer: selector::DeltaWriter<'a>,
    // Only AtBase opens this read transaction. It never supplies finalization authority.
    starting: Option<QuerySnapshot>,
    proof: IndexedRefreshProof,
    delta: RetainedDelta,
    phase: IndexedRefreshPhase,
    selected_bytes_remaining: Cell<usize>,
}

fn recovery(message: &str) -> WikiError {
    WikiError::new(ErrorCode::RecoveryRequired, message)
}
fn budget(message: &str) -> WikiError {
    WikiError::new(ErrorCode::BudgetExceeded, message)
}
fn retained_error(mut error: WikiError, change: &PreparedChange) -> WikiError {
    let reference =
        serde_json::json!({"change_id":change.change_id,"manifest_hash":change.manifest_hash});
    if let Some(details) = error.details.as_object_mut() {
        details.insert("change".into(), reference);
    } else {
        error.details = serde_json::json!({"change":reference,"cause_details":error.details});
    }
    error
}
fn publication(snapshot: &ReadSnapshot) -> Result<Publication> {
    let binding = snapshot
        .publication()
        .ok_or_else(|| recovery("refresh requires a published epoch"))?;
    Ok(Publication {
        file_id: binding.file_id.clone(),
        epoch: snapshot.generation,
    })
}
pub(crate) fn delta_path(change: &PreparedChange) -> Result<VaultRelativePath> {
    VaultRelativePath::new(format!("changes/{}/indexed-delta.json", change.change_id))
}
fn intended(
    base: &ReadSnapshot,
    change: &PreparedChange,
    delta_hash: &Blake3Hash,
    version: u32,
) -> Result<ReadSnapshot> {
    let binding = base
        .publication()
        .ok_or_else(|| recovery("refresh base is not a published epoch"))?;
    let hash = Blake3Hash::digest(
        serde_json::to_vec(&(
            if version <= 2 {
                "lwiki.source-refresh-publication.v1"
            } else {
                "lwiki.normalized-write-publication.v1"
            },
            base,
            change,
            delta_hash,
        ))
        .map_err(|error| WikiError::invalid(error.to_string()))?,
    );
    ReadSnapshot::published(
        base.generation
            .checked_add(1)
            .ok_or_else(|| recovery("refresh epoch exhausted"))?,
        base.parser_fingerprint.clone(),
        binding.file_id.clone(),
        hash,
    )
}

/// A retained v3 delta may replace only owners admitted by its selected proof.
/// Captured-content lifecycle metadata is bound by its authenticated Revision;
/// changing eligibility does not require rereading an immutable payload.
fn require_selected_delta_scope(
    rows: &CatalogDelta,
    after: &BTreeMap<&VaultRelativePath, &ExpectedState>,
) -> Result<()> {
    use super::{normalized_delta::DocumentMutation, policy_facts::PolicyRow};
    let selected = |path: &VaultRelativePath| {
        if matches!(after.get(path), Some(ExpectedState::Hash(_)))
            || (matches!(after.get(path), Some(ExpectedState::Absent))
                && rows
                    .documents
                    .iter()
                    .any(|d| matches!(d, DocumentMutation::MovePage { from, .. } if from == path)))
        {
            Ok(())
        } else {
            Err(recovery(
                "indexed write row owner is outside selected after state",
            ))
        }
    };
    let exact = |path: &VaultRelativePath, hash: &Blake3Hash| {
        if after.get(path).copied() == Some(&ExpectedState::Hash(hash.clone())) {
            Ok(())
        } else {
            Err(recovery(
                "indexed write row hash differs from selected after state",
            ))
        }
    };
    for row in &rows.records {
        exact(&row.path, &row.hash)?;
    }
    let record_ids: BTreeSet<_> = rows.records.iter().map(|row| row.record.id()).collect();
    let identity = |id: &RecordId| {
        if record_ids.contains(id) {
            Ok(())
        } else {
            Err(recovery(
                "indexed write derived owner lacks a selected canonical record",
            ))
        }
    };
    for row in &rows.graph {
        identity(&row.target_id)?;
    }
    for row in &rows.revisions {
        identity(&row.source_id)?;
        identity(&row.revision_id)?;
    }
    for owner in rows
        .claims
        .iter()
        .map(|row| &row.path)
        .chain(rows.diagnostics.iter().map(|row| &row.path))
        .chain(rows.links.iter().map(|row| &row.path))
    {
        selected(owner)?;
    }
    for document in &rows.documents {
        match document {
            DocumentMutation::Put { row } => exact(&row.path, &row.hash)?,
            DocumentMutation::MovePage { from, row, .. } => {
                exact(&row.path, &row.hash)?;
                if after.get(from).copied() != Some(&ExpectedState::Absent) {
                    return Err(recovery(
                        "Page move old path is not retired in selected proof",
                    ));
                }
            }
            DocumentMutation::Metadata { path, .. } => {
                let revision_bound = rows.records.iter().any(|row| {
                    row.record.kind() == RecordKind::Revision
                        && row
                            .record
                            .string("wiki_source_id")
                            .zip(row.record.string("wiki_content_path"))
                            .is_some_and(|(source, content)| {
                                path.as_str()
                                    == format!(
                                        "sources/{source}/revisions/{}/{content}",
                                        row.record.id()
                                    )
                            })
                });
                if !revision_bound {
                    selected(path)?;
                }
            }
        }
    }
    if let Some(facts) = &rows.facts {
        for id in facts
            .records
            .iter()
            .map(|row| &row.record_id)
            .chain(facts.registry.iter().map(|row| &row.record_id))
            .chain(facts.edge_inserts.iter().map(|row| &row.owner_id))
            .chain(facts.edge_deletes.iter().map(|row| &row.owner_id))
        {
            identity(id)?;
        }
        for path in facts
            .links
            .iter()
            .map(|row| &row.path)
            .chain(facts.registry.iter().map(|row| &row.path))
        {
            selected(path)?;
        }
        if let Some(policy) = &facts.policy {
            for member in &policy.memberships {
                exact(&member.path, &member.hash)?;
            }
            for row in policy
                .replacements
                .iter()
                .flat_map(|replacement| &replacement.rows)
            {
                if let PolicyRow::ReadPath { path, hash, .. } = row {
                    exact(path, hash)?;
                }
            }
        }
    }
    Ok(())
}

impl RetainedDelta {
    fn require_refresh_batch_rows(
        &self,
        refreshes: &[crate::changes::indexed_refresh::IndexedRefreshTarget],
        dependent_sources: &[crate::changes::indexed_refresh::IndexedRefreshSourceDependency],
        manifest: &crate::changes::ChangeManifest,
        engine: &ChangeEngine,
    ) -> Result<()> {
        use super::{normalized_delta::DocumentMutation, row_projection};
        use crate::records::parse_note;
        let sources: BTreeSet<_> = refreshes.iter().map(|target| &target.source_id).collect();
        let mut expected_paths = BTreeSet::new();
        let mut expected_documents = BTreeMap::new();
        let mut allocated = BTreeMap::new();
        let mut fresh = BTreeSet::new();
        let mut authenticated_revisions = BTreeSet::new();
        let remaining = Cell::new(MAX_SELECTED_BYTES);
        let charge = |bytes: usize| -> Result<()> {
            remaining.set(remaining.get().checked_sub(bytes).ok_or_else(|| {
                budget("refresh batch retained verification exceeds byte ceiling")
            })?);
            Ok(())
        };
        let payload_note = |index: usize, side: &str| -> Result<crate::records::ParsedNote> {
            let operation = &manifest.operations[index];
            let (expected, reference) = if side == "before" {
                (&operation.before, &operation.before_payload)
            } else {
                (&operation.after, &operation.after_payload)
            };
            let bytes = engine
                .verify_payload_with_limit(
                    &manifest.change_id,
                    index,
                    side,
                    &operation.target,
                    (expected, reference),
                    remaining.get(),
                )?
                .ok_or_else(|| {
                    recovery("refresh batch lacks its exact Source or revision payload")
                })?;
            charge(bytes.len())?;
            Ok(parse_note(&bytes))
        };
        let selected_bytes = |path: &VaultRelativePath| -> Result<Vec<u8>> {
            let expected = self
                .after
                .iter()
                .find(|dependency| &dependency.path == path)
                .map(|dependency| &dependency.expected)
                .ok_or_else(|| recovery("refresh batch payload lacks a selected guard"))?;
            let bytes = if let Some((index, operation)) = manifest
                .operations
                .iter()
                .enumerate()
                .find(|(_, operation)| &operation.target == path)
            {
                engine.verify_payload_with_limit(
                    &manifest.change_id,
                    index,
                    "proposed",
                    path,
                    (&operation.after, &operation.after_payload),
                    remaining.get(),
                )?
            } else {
                // Existing revision trees are immutable. Completed replay must
                // never authenticate an unchanged row against today's Source.
                if !path.as_str().starts_with("sources/") || !path.as_str().contains("/revisions/")
                {
                    return Err(recovery(
                        "refresh batch read leaves its immutable revision trees",
                    ));
                }
                read_bounded(engine.fs(), path, remaining.get().min(MAX_PAYLOAD_BYTES))?
            }
            .ok_or_else(|| recovery("refresh batch selected immutable payload is missing"))?;
            charge(bytes.len())?;
            if *expected != ExpectedState::Hash(Blake3Hash::digest(&bytes)) {
                return Err(recovery(
                    "refresh batch selected immutable payload differs from its guard",
                ));
            }
            Ok(bytes)
        };
        for (ordinal, target) in refreshes.iter().enumerate() {
            let source_path =
                VaultRelativePath::new(format!("sources/{}/source.md", target.source_id))?;
            let source = self
                .rows
                .records
                .iter()
                .find(|row| {
                    row.record.id() == &target.source_id
                        && row.record.kind() == RecordKind::Source
                        && row.path == source_path
                })
                .ok_or_else(|| recovery("refresh batch omits a declared Source row"))?;
            if source.record.string("wiki_status") != Some("active")
                || source.record.string("wiki_current_revision")
                    != Some(target.revision_id.as_str())
                || !scan::list(&source.record, "wiki_revisions")
                    .contains(&target.revision_id.to_string())
                || !self.after.iter().any(|dependency| {
                    dependency.path == source_path
                        && dependency.expected == ExpectedState::Hash(source.hash.clone())
                })
            {
                return Err(recovery(
                    "refresh batch Source identity, status, head or selected guard differs",
                ));
            }
            let source_operation = manifest
                .operations
                .iter()
                .position(|operation| operation.target == source_path);
            if target.no_op {
                let bytes = target
                    .unchanged_source
                    .as_ref()
                    .ok_or_else(|| recovery("refresh batch no-op lacks retained Source bytes"))?;
                let note = parse_note(bytes.as_bytes());
                if source_operation.is_some()
                    || note.source_hash != source.hash
                    || note.canonical.as_ref() != Some(&source.record)
                    || !self.before.iter().any(|dependency| {
                        dependency.path == source_path
                            && dependency.expected == ExpectedState::Hash(source.hash.clone())
                    })
                {
                    return Err(recovery(
                        "refresh batch no-op bytes, row or unchanged guard differs",
                    ));
                }
                charge(bytes.len())?;
                let inventory = scan::list(&source.record, "wiki_revisions");
                if inventory.iter().collect::<BTreeSet<_>>().len() != inventory.len() {
                    return Err(recovery(
                        "refresh batch no-op revision inventory contains duplicates",
                    ));
                }
                continue;
            }
            if target.unchanged_source.is_some() {
                return Err(recovery(
                    "changed refresh batch member carries a no-op payload",
                ));
            }
            let source_index = source_operation
                .ok_or_else(|| recovery("refresh batch omits a declared Source mutation"))?;
            let operation = &manifest.operations[source_index];
            let before = payload_note(source_index, "before")?;
            let after = payload_note(source_index, "proposed")?;
            let prior = before
                .canonical
                .as_ref()
                .ok_or_else(|| recovery("refresh batch before-Source is not canonical"))?;
            if prior.id() != &target.source_id
                || prior.kind() != RecordKind::Source
                || prior.string("wiki_status") != Some("active")
                || prior.string("wiki_current_revision")
                    != Some(target.previous_revision_id.as_str())
                || operation.before != ExpectedState::Hash(before.source_hash.clone())
                || operation.after != ExpectedState::Hash(source.hash.clone())
                || after.source_hash != source.hash
                || after.canonical.as_ref() != Some(&source.record)
                || before.body() != after.body()
            {
                return Err(recovery(
                    "refresh batch retained Source before/after bytes differ from its target",
                ));
            }
            let fixed = |name: &&String| {
                !matches!(
                    name.as_str(),
                    "title" | "wiki_current_revision" | "wiki_revision" | "wiki_revisions"
                )
            };
            if prior
                .fields()
                .iter()
                .filter(|(name, _)| fixed(name))
                .ne(source
                    .record
                    .fields()
                    .iter()
                    .filter(|(name, _)| fixed(name)))
            {
                return Err(recovery("refresh batch changes fixed Source metadata"));
            }
            let old_retained = scan::list(prior, "wiki_revisions");
            if !old_retained.contains(&target.previous_revision_id.to_string()) {
                return Err(recovery("refresh batch previous head is not retained"));
            }
            let mut retained = old_retained.clone();
            if target.reused {
                if !retained.contains(&target.revision_id.to_string()) {
                    return Err(recovery("refresh batch reuses an unretained revision"));
                }
            } else {
                if retained.contains(&target.revision_id.to_string()) {
                    return Err(recovery(
                        "refresh batch fresh revision was already retained",
                    ));
                }
                retained.push(target.revision_id.to_string());
                allocated.insert(format!("revision_{ordinal}"), target.revision_id.clone());
                fresh.insert((&target.source_id, &target.revision_id));
            }
            if scan::list(&source.record, "wiki_revisions") != retained
                || retained.iter().collect::<BTreeSet<_>>().len() != retained.len()
            {
                return Err(recovery(
                    "refresh batch revision inventory is not exact append or reuse",
                ));
            }
            expected_documents.insert(
                source_path.clone(),
                row_projection::canonical_document(&source_path, &after, Some(source)),
            );
            expected_paths.insert(source_path);
            if !target.reused {
                let root = format!(
                    "sources/{}/revisions/{}",
                    target.source_id, target.revision_id
                );
                let revision_path = VaultRelativePath::new(format!("{root}/revision.md"))?;
                let revision_index = manifest
                    .operations
                    .iter()
                    .position(|operation| operation.target == revision_path)
                    .ok_or_else(|| recovery("refresh batch lacks its declared fresh revision"))?;
                let revision_note = payload_note(revision_index, "proposed")?;
                let revision = revision_note
                    .canonical
                    .as_ref()
                    .ok_or_else(|| recovery("refresh batch fresh revision is not canonical"))?;
                let row = self
                    .rows
                    .records
                    .iter()
                    .find(|row| {
                        row.record.id() == &target.revision_id
                            && row.record.kind() == RecordKind::Revision
                    })
                    .ok_or_else(|| recovery("refresh batch lacks fresh revision row"))?;
                if revision.id() != &target.revision_id
                    || revision.kind() != RecordKind::Revision
                    || revision.string("wiki_source_id") != Some(target.source_id.as_str())
                    || row.path != revision_path
                    || row.hash != revision_note.source_hash
                    || row.record != *revision
                    || !revision_note.body().is_empty()
                {
                    return Err(recovery(
                        "refresh batch fresh revision owner, path or metadata differs",
                    ));
                }
                authenticated_revisions.insert(target.revision_id.clone());
                expected_documents.insert(
                    revision_path.clone(),
                    row_projection::canonical_document(&revision_path, &revision_note, Some(row)),
                );
                expected_paths.insert(revision_path);
                for field in ["wiki_original_path", "wiki_content_path"] {
                    if let Some(name) = revision.string(field) {
                        let path = super::source_projection::asset_path(row, name)?;
                        if path.as_str().rsplit_once('/').map(|(parent, _)| parent)
                            != Some(root.as_str())
                        {
                            return Err(recovery(
                                "refresh batch payload leaves its exact fresh revision tree",
                            ));
                        }
                        let hash_field = if field == "wiki_original_path" {
                            "wiki_original_hash"
                        } else {
                            "wiki_content_hash"
                        };
                        let declared_hash = revision
                            .string(hash_field)
                            .map(Blake3Hash::new)
                            .transpose()?
                            .ok_or_else(|| {
                                recovery("refresh batch revision payload hash is missing")
                            })?;
                        if !manifest.operations.iter().any(|operation| {
                            operation.target == path
                                && operation.after == ExpectedState::Hash(declared_hash.clone())
                        }) {
                            return Err(recovery(
                                "refresh batch revision payload differs from its declared hash",
                            ));
                        }
                        expected_paths.insert(path);
                    } else if field == "wiki_original_path" {
                        return Err(recovery(
                            "refresh batch fresh revision lacks original payload",
                        ));
                    }
                }
                let identity = self
                    .rows
                    .revisions
                    .iter()
                    .find(|identity| {
                        identity.source_id == target.source_id
                            && identity.revision_id == target.revision_id
                    })
                    .ok_or_else(|| recovery("refresh batch omits fresh revision identity"))?;
                if identity.retained_ordinal != old_retained.len()
                    || Some(identity.original_hash.as_str())
                        != revision.string("wiki_original_hash")
                    || identity.content_hash.as_ref().map(Blake3Hash::as_str)
                        != revision.string("wiki_content_hash")
                    || Some(identity.extractor_fingerprint.as_str())
                        != revision.string("wiki_extractor_fingerprint")
                    || Some(identity.extraction_status.as_str())
                        != revision.string("wiki_extraction_status")
                {
                    return Err(recovery(
                        "refresh batch fresh revision identity differs from retained metadata",
                    ));
                }
            }
            for revision_id in BTreeSet::from([&target.previous_revision_id, &target.revision_id]) {
                let row = self
                    .rows
                    .records
                    .iter()
                    .find(|row| row.record.id() == revision_id)
                    .ok_or_else(|| recovery("refresh batch changed head omits its Revision row"));
                // Title-only refreshes do not replace the unchanged Revision.
                let row = match row {
                    Ok(row) => row,
                    Err(_) if target.previous_revision_id == target.revision_id => continue,
                    Err(error) => return Err(error),
                };
                let path = VaultRelativePath::new(format!(
                    "sources/{}/revisions/{revision_id}/revision.md",
                    target.source_id
                ))?;
                let note = if revision_id == &target.revision_id && !target.reused {
                    // Its exact retained payload was checked above.
                    None
                } else {
                    Some(parse_note(&selected_bytes(&path)?))
                };
                if row.path != path
                    || row.record.kind() != RecordKind::Revision
                    || row.record.string("wiki_source_id") != Some(target.source_id.as_str())
                    || note.as_ref().is_some_and(|note| {
                        note.source_hash != row.hash || note.canonical.as_ref() != Some(&row.record)
                    })
                {
                    return Err(recovery(
                        "refresh batch retained Revision identity or owner differs from immutable bytes",
                    ));
                }
                authenticated_revisions.insert(revision_id.clone());
                if revision_id == &target.revision_id
                    && target.previous_revision_id != target.revision_id
                    && row.record.string("wiki_extraction_status") == Some("complete")
                {
                    let path = super::source_projection::asset_path(
                        row,
                        row.record.string("wiki_content_path").ok_or_else(|| {
                            recovery("refresh batch complete head lacks content path")
                        })?,
                    )?;
                    let bytes = selected_bytes(&path)?;
                    if Some(Blake3Hash::digest(&bytes).as_str())
                        != row.record.string("wiki_content_hash")
                    {
                        return Err(recovery(
                            "refresh batch current content hash differs from its Revision",
                        ));
                    }
                    let text = String::from_utf8(bytes)
                        .map_err(|_| recovery("refresh batch current content is not UTF-8"))?;
                    expected_documents.insert(
                        path.clone(),
                        row_projection::captured_content_document(
                            path,
                            target.source_id.clone(),
                            row,
                            text,
                        ),
                    );
                }
            }
        }
        for row in self
            .rows
            .records
            .iter()
            .filter(|row| row.record.kind() == RecordKind::Revision)
        {
            if authenticated_revisions.contains(row.record.id()) {
                continue;
            }
            let source_id = row.record.string("wiki_source_id").ok_or_else(|| {
                recovery("refresh batch immutable Revision lacks its Source owner")
            })?;
            let path = VaultRelativePath::new(format!(
                "sources/{}/revisions/{}/revision.md",
                source_id,
                row.record.id()
            ))?;
            let note = parse_note(&selected_bytes(&path)?);
            if row.path != path
                || note.source_hash != row.hash
                || note.canonical.as_ref() != Some(&row.record)
            {
                return Err(recovery(
                    "refresh batch unchanged Revision row differs from immutable bytes",
                ));
            }
        }
        let actual_documents: BTreeMap<_, _> = self
            .rows
            .documents
            .iter()
            .filter_map(|mutation| {
                if let DocumentMutation::Put { row } = mutation {
                    Some((&row.path, row))
                } else {
                    None
                }
            })
            .collect();
        if actual_documents.len() != expected_documents.len()
            || expected_documents
                .iter()
                .any(|(path, expected)| actual_documents.get(path) != Some(&expected))
        {
            return Err(recovery(
                "refresh batch document rows differ from exact retained bytes",
            ));
        }
        let dependent_rows: BTreeMap<_, _> = self
            .rows
            .records
            .iter()
            .filter(|row| {
                row.record.kind() == RecordKind::Source && !sources.contains(row.record.id())
            })
            .map(|row| (row.record.id(), row))
            .collect();
        if dependent_rows.len() != dependent_sources.len() {
            return Err(recovery(
                "refresh batch dependent Source witness set differs from its rows",
            ));
        }
        for dependency in dependent_sources {
            let row = dependent_rows
                .get(&dependency.source_id)
                .ok_or_else(|| recovery("refresh batch dependent Source witness lacks its row"))?;
            let path =
                VaultRelativePath::new(format!("sources/{}/source.md", dependency.source_id))?;
            charge(dependency.source_bytes.len())?;
            let note = parse_note(dependency.source_bytes.as_bytes());
            let expected = ExpectedState::Hash(note.source_hash.clone());
            if row.path != path
                || row.hash != note.source_hash
                || note.canonical.as_ref() != Some(&row.record)
                || !self
                    .before
                    .iter()
                    .any(|guard| guard.path == path && guard.expected == expected)
                || !self
                    .after
                    .iter()
                    .any(|guard| guard.path == path && guard.expected == expected)
                || manifest
                    .operations
                    .iter()
                    .any(|operation| operation.target == path)
            {
                return Err(recovery(
                    "refresh batch dependent Source bytes, row or unchanged guards differ",
                ));
            }
        }
        if manifest.allocated_ids != allocated
            || self.rows.revisions.len() != fresh.len()
            || self.rows.owners.len() != fresh.len()
            || self.rows.owners.iter().any(|owner| {
                !fresh.iter().any(|(source, revision)| {
                    owner.key.source_component == source.as_str()
                        && owner.key.revision_component == revision.as_str()
                })
            })
            || manifest.operations.len() != expected_paths.len()
            || manifest.operations.iter().any(|operation| {
                !expected_paths.contains(&operation.target)
                    || (operation.target.as_str().contains("/revisions/")
                        && operation.before != ExpectedState::Absent)
            })
        {
            return Err(recovery(
                "refresh batch allocations, owners or write set cross the exact declared boundary",
            ));
        }
        Ok(())
    }

    fn require_operation_rows(
        &self,
        manifest: &crate::changes::ChangeManifest,
        engine: &ChangeEngine,
    ) -> Result<()> {
        let Some(operation) = &self.operation else {
            // Exact historical v2 admission is intentionally unchanged.
            return Ok(());
        };
        let written = |id: &RecordId, kind: RecordKind| {
            self.rows.records.iter().find(|row| {
                row.record.id() == id
                    && row.record.kind() == kind
                    && manifest.operations.iter().any(|op| {
                        op.target == row.path && op.after == ExpectedState::Hash(row.hash.clone())
                    })
            }).ok_or_else(|| recovery("indexed operation identity, kind, or after-image differs from its manifest"))
        };
        match operation {
            IndexedWriteOperation::SourceRefreshBatch {
                refreshes,
                dependent_sources,
            } => {
                self.require_refresh_batch_rows(refreshes, dependent_sources, manifest, engine)?;
            }
            IndexedWriteOperation::SourceWithdraw { source_id } => {
                let source = written(source_id, RecordKind::Source)?;
                if manifest.operations.len() != 1
                    || manifest.operations[0].target != source.path
                    || !self.rows.owners.is_empty()
                {
                    return Err(recovery("withdrawal crosses its existing Source boundary"));
                }
                let op = &manifest.operations[0];
                let before = engine
                    .verify_payload(
                        &manifest.change_id,
                        0,
                        "before",
                        &op.target,
                        &op.before,
                        &op.before_payload,
                    )?
                    .ok_or_else(|| recovery("withdrawal lacks an existing before-image"))?;
                let after = engine
                    .verify_payload(
                        &manifest.change_id,
                        0,
                        "proposed",
                        &op.target,
                        &op.after,
                        &op.after_payload,
                    )?
                    .ok_or_else(|| recovery("withdrawal lacks a retained after-image"))?;
                let before = crate::records::parse_note(&before);
                let after = crate::records::parse_note(&after);
                let old = before
                    .canonical
                    .as_ref()
                    .filter(|record| {
                        record.id() == source_id && record.kind() == RecordKind::Source
                    })
                    .ok_or_else(|| recovery("withdrawal before-image identity differs"))?;
                let new = after
                    .canonical
                    .as_ref()
                    .filter(|record| *record == &source.record)
                    .ok_or_else(|| recovery("withdrawal row differs from retained bytes"))?;
                let allowed = ["wiki_status", "wiki_withdrawn_at", "wiki_withdrawal_reason"];
                if old.string("wiki_status") != Some("active")
                    || new.string("wiki_status") != Some("withdrawn")
                    || new.string("wiki_withdrawn_at").is_none()
                    || new
                        .string("wiki_withdrawal_reason")
                        .is_none_or(|reason| reason.trim().is_empty() || reason.len() > 4096)
                    || before.body() != after.body()
                    || old
                        .fields()
                        .iter()
                        .filter(|(key, _)| !allowed.contains(&key.as_str()))
                        .collect::<BTreeMap<_, _>>()
                        != new
                            .fields()
                            .iter()
                            .filter(|(key, _)| !allowed.contains(&key.as_str()))
                            .collect::<BTreeMap<_, _>>()
                {
                    return Err(recovery(
                        "withdrawal changes fields outside status, time and reason",
                    ));
                }
            }
            IndexedWriteOperation::JobBatch {
                records,
                checkpoint,
                ..
            } => {
                let mut draft = crate::changes::ChangeDraft {
                    title: manifest.title.clone(),
                    origin: manifest.origin.clone(),
                    inverse_of: manifest.inverse_of.clone(),
                    allocated_ids: manifest.allocated_ids.clone(),
                    read_preconditions: manifest.read_preconditions.clone(),
                    operations: vec![],
                };
                for (index, op) in manifest.operations.iter().enumerate() {
                    let bytes = engine
                        .verify_payload(
                            &manifest.change_id,
                            index,
                            "proposed",
                            &op.target,
                            &op.after,
                            &op.after_payload,
                        )?
                        .ok_or_else(|| recovery("job retained payload missing"))?;
                    draft.operations.push(crate::changes::ExpectedWrite {
                        target: op.target.clone(),
                        expected: op.before.clone(),
                        proposed: Some(bytes),
                        apply_after: op
                            .apply_after
                            .iter()
                            .map(|index| manifest.operations[*index].target.clone())
                            .collect(),
                    });
                }
                crate::jobs::checkpoint::validate_job_payloads(
                    engine.fs(),
                    &self.vault_id,
                    operation,
                    &draft,
                )?;
                for record in records {
                    let row = written(&record.id, record.kind)?;
                    let bytes = draft
                        .operations
                        .iter()
                        .find(|op| op.target == record.path)
                        .and_then(|op| op.proposed.as_deref())
                        .ok_or_else(|| recovery("job row payload absent"))?;
                    let parsed = crate::records::parse_note(bytes);
                    let document =
                        super::row_projection::canonical_document(&record.path, &parsed, Some(row));
                    if row.path != record.path || parsed.canonical.as_ref() != Some(&row.record)
                        || !self.rows.documents.iter().any(|d| matches!(d,
                            super::normalized_delta::DocumentMutation::Put { row } if row == &document))
                    {
                        return Err(recovery("job record/document rows differ from exact canonical after-image"));
                    }
                }
                if !self.rows.owners.is_empty() || !self.rows.revisions.is_empty()
                    || checkpoint.as_ref().is_some_and(|c| self.rows.documents.iter().any(|d| {
                        matches!(d, super::normalized_delta::DocumentMutation::Put { row } if row.path == c.path)
                    }))
                    || self.rows.documents.iter().any(|d| matches!(d,
                        super::normalized_delta::DocumentMutation::Put { row }
                            if matches!(row.kind, Some(RecordKind::Run | RecordKind::RunEvent))
                                && (!row.body.is_empty() || !row.headings.is_empty())))
                {
                    return Err(recovery("job publication leaks assets or bookkeeping into document evidence"));
                }
            }
            IndexedWriteOperation::PageBatch { pages } => {
                for page in pages {
                    if written(&page.id, RecordKind::Page)?.path != page.path {
                        return Err(recovery("page operation identity is bound to another path"));
                    }
                }
            }
            IndexedWriteOperation::PageRename {
                page_id,
                from,
                to,
                from_hash,
                rewritten_paths,
            } => {
                let moved = written(page_id, RecordKind::Page)?;
                if moved.path != *to
                    || !self.rows.owners.is_empty()
                    || !self.rows.revisions.is_empty()
                {
                    return Err(recovery(
                        "Page rename crosses its authored identity boundary",
                    ));
                }
                let read_note =
                    |path: &VaultRelativePath, side: &str| -> Result<crate::records::ParsedNote> {
                        let (index, op) = manifest
                            .operations
                            .iter()
                            .enumerate()
                            .find(|(_, op)| &op.target == path)
                            .ok_or_else(|| recovery("Page rename owner is absent from manifest"))?;
                        let (payload, state, retained_side) = if side == "before" {
                            (&op.before_payload, &op.before, "before")
                        } else {
                            (&op.after_payload, &op.after, "proposed")
                        };
                        let bytes = engine
                            .verify_payload(
                                &manifest.change_id,
                                index,
                                retained_side,
                                path,
                                state,
                                payload,
                            )?
                            .ok_or_else(|| recovery("Page rename owner payload is absent"))?;
                        Ok(crate::records::parse_note(&bytes))
                    };
                let before = read_note(from, "before")?;
                let after = read_note(to, "after")?;
                let old = before
                    .canonical
                    .as_ref()
                    .ok_or_else(|| recovery("Page rename before envelope is not adopted"))?;
                let new = after
                    .canonical
                    .as_ref()
                    .ok_or_else(|| recovery("Page rename destination envelope is not adopted"))?;
                if before.source_hash != *from_hash
                    || old.id() != page_id
                    || old.kind() != RecordKind::Page
                    || new != &moved.record
                    || after.source_hash != moved.hash
                {
                    return Err(recovery(
                        "Page rename retained identity or author bytes differ",
                    ));
                }
                // Page has no schema-defined typed Page companion. Preserve
                // every envelope field except the pure parser rewrite helper's
                // same-ID navigation strings, which carry no new identity.
                let changes = crate::records::link_rewrite::rewrite_companions_selected(
                    &before,
                    &mut |_| Ok(crate::records::LinkResolution::Missing),
                    page_id,
                    to,
                )?;
                let mut fields = old.fields().clone();
                fields.extend(changes);
                if &fields != new.fields() {
                    return Err(recovery("Page rename changes authored envelope fields"));
                }
                for path in rewritten_paths {
                    let prior = read_note(path, "before")?;
                    let next = read_note(path, "after")?;
                    let parts: Vec<_> = path.as_str().split('/').collect();
                    let immutable_path = parts.len() >= 4
                        && unicase::UniCase::unicode(parts[0]).to_folded_case() == "sources"
                        && unicase::UniCase::unicode(parts[2]).to_folded_case() == "revisions";
                    let immutable_kind = prior
                        .fields
                        .as_ref()
                        .and_then(|fields| fields.get("wiki_kind"))
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|kind| {
                            matches!(kind, "revision" | "extraction_packet" | "run_event")
                        });
                    if immutable_path
                        || immutable_kind
                        || prior.canonical != next.canonical
                        || prior.fields != next.fields
                    {
                        return Err(recovery(
                            "Page rename rewrites immutable ownership or changes incoming envelope fields",
                        ));
                    }
                    let row = self
                        .rows
                        .documents
                        .iter()
                        .find_map(|document| match document {
                            super::normalized_delta::DocumentMutation::Put { row }
                                if &row.path == path =>
                            {
                                Some(row)
                            }
                            _ => None,
                        })
                        .ok_or_else(|| {
                            recovery("Page rename incoming owner lacks document replacement")
                        })?;
                    if row.hash != next.source_hash
                        || row.raw_text.as_bytes() != next.raw.as_slice()
                        || row.owner_revision.is_some()
                        || row.source_id.is_some()
                    {
                        return Err(recovery(
                            "Page rename incoming document differs from retained bytes",
                        ));
                    }
                    if let Some(id) = &row.record_id {
                        let record = next.canonical.as_ref().ok_or_else(|| {
                            recovery("Page rename incoming adopted envelope differs")
                        })?;
                        if written(id, record.kind())?.record != *record {
                            return Err(recovery(
                                "Page rename incoming canonical row differs from bytes",
                            ));
                        }
                    }
                }
            }
            IndexedWriteOperation::SourceRefresh { source_id } => {
                let source = written(source_id, RecordKind::Source)?;
                let head = manifest
                    .operations
                    .iter()
                    .find(|op| op.target == source.path)
                    .unwrap();
                if !matches!(head.before, ExpectedState::Hash(_))
                    || self
                        .rows
                        .owners
                        .iter()
                        .any(|owner| owner.key.source_component != source_id.as_str())
                    || manifest.operations.iter().any(|op| {
                        op.target != source.path
                            && !op
                                .target
                                .as_str()
                                .starts_with(&format!("sources/{source_id}/revisions/"))
                    })
                {
                    return Err(recovery(
                        "source refresh operation crosses its existing source boundary",
                    ));
                }
            }
            IndexedWriteOperation::SourceCapture { .. }
            | IndexedWriteOperation::SourceCaptureBatch { .. } => {
                let captures = operation.capture_targets();
                if self.rows.revisions.len() != captures.len() {
                    return Err(recovery(
                        "capture revision rows differ from declared fresh trees",
                    ));
                }
                if matches!(operation, IndexedWriteOperation::SourceCaptureBatch { .. }) {
                    let allocated: BTreeMap<_, _> = captures
                        .iter()
                        .enumerate()
                        .flat_map(|(index, capture)| {
                            [
                                (format!("source_{index}"), capture.source_id.clone()),
                                (format!("revision_{index}"), capture.revision_id.clone()),
                            ]
                        })
                        .collect();
                    if manifest.allocated_ids != allocated {
                        return Err(recovery(
                            "capture batch allocations differ from declared pair order",
                        ));
                    }
                }
                let mut expected_paths = BTreeSet::new();
                for capture in captures {
                    let source_id = &capture.source_id;
                    let revision_id = &capture.revision_id;
                    let source = written(source_id, RecordKind::Source)?;
                    let revision = written(revision_id, RecordKind::Revision)?;
                    let source_path = format!("sources/{source_id}/source.md");
                    let root = format!("sources/{source_id}/revisions/{revision_id}");
                    let revision_path = format!("{root}/revision.md");
                    if source.path.as_str() != source_path
                        || revision.path.as_str() != revision_path
                        || source.record.string("wiki_current_revision")
                            != Some(revision_id.as_str())
                        || revision.record.string("wiki_source_id") != Some(source_id.as_str())
                        || super::scan::list(&source.record, "wiki_revisions")
                            != [revision_id.to_string()]
                    {
                        return Err(recovery(
                            "capture operation identity differs from its fresh tree",
                        ));
                    }
                    let identity = self
                        .rows
                        .revisions
                        .iter()
                        .find(|row| &row.source_id == source_id && &row.revision_id == revision_id)
                        .ok_or_else(|| recovery("capture lacks its declared revision metadata"))?;
                    if identity.retained_ordinal != 0
                        || Some(identity.original_hash.as_str())
                            != revision.record.string("wiki_original_hash")
                        || identity.content_hash.as_ref().map(Blake3Hash::as_str)
                            != revision.record.string("wiki_content_hash")
                        || Some(identity.extractor_fingerprint.as_str())
                            != revision.record.string("wiki_extractor_fingerprint")
                        || Some(identity.extraction_status.as_str())
                            != revision.record.string("wiki_extraction_status")
                    {
                        return Err(recovery(
                            "capture revision metadata differs from its written envelope",
                        ));
                    }
                    expected_paths.extend([
                        source_path,
                        revision_path,
                        format!("{root}/original.bin"),
                    ]);
                    if revision.record.string("wiki_extraction_status") == Some("complete") {
                        expected_paths.insert(format!("{root}/content.md"));
                    } else if revision.record.string("wiki_extraction_status")
                        != Some("unsupported")
                    {
                        return Err(recovery("capture operation has invalid extraction state"));
                    }
                }
                if manifest.operations.len() != expected_paths.len()
                    || manifest.operations.iter().any(|op| {
                        op.before != ExpectedState::Absent
                            || matches!(op.after, ExpectedState::Absent)
                            || !expected_paths.contains(op.target.as_str())
                    })
                    || expected_paths.iter().any(|path| {
                        !manifest
                            .operations
                            .iter()
                            .any(|op| op.target.as_str() == path)
                    })
                {
                    return Err(recovery(
                        "capture operation writes outside its exact fresh trees",
                    ));
                }
            }
        }
        Ok(())
    }

    fn require_bound(
        &self,
        catalog: &Catalog,
        engine: &ChangeEngine,
        proof: &IndexedRefreshProof,
    ) -> Result<()> {
        let (manifest, hash) = engine.load_manifest_structure(&proof.change.change_id)?;
        proof.validate_manifest(&manifest)?;
        if hash != proof.change.manifest_hash
            || (!matches!(self.version, 2 | 3) && !(cfg!(test) && self.version == 1))
            || (self.version == 2 && self.rows.version != 2)
            || (self.version == 3 && self.rows.version != 3)
            || (self.version <= 2
                && (proof.version != 2 || self.operation.is_some() || self.source_id.is_none()))
            || (self.version == 3
                && (proof.version != 3 || self.operation.is_none() || self.source_id.is_some()))
            || proof.vault_id != catalog.vault_id
            || self.vault_id != proof.vault_id
            || self.source_id != proof.source_id
            || self.operation != proof.operation
            || self.change != proof.change
            || self.base != proof.base
            || self.before != proof.before
            || self.after != proof.after
            || proof.base.parser_fingerprint != scan::parser_fingerprint()
            || intended(&proof.base, &proof.change, &proof.delta_hash, self.version)?
                != proof.intended
        {
            return Err(recovery(
                "refresh delta, manifest, or replay version differs from retained proof",
            ));
        }
        self.rows.validate()?;
        self.require_operation_rows(&manifest, engine)?;
        if self.rows.owners != engine.manifest_revision_owners(&proof.change)? {
            return Err(recovery(
                "refresh delta owners differ from exact manifest roots",
            ));
        }
        match &self.operation {
            Some(
                IndexedWriteOperation::PageBatch { .. }
                | IndexedWriteOperation::JobBatch { .. }
                | IndexedWriteOperation::PageRename { .. }
                | IndexedWriteOperation::SourceWithdraw { .. },
            ) if !self.rows.owners.is_empty() => {
                return Err(recovery(
                    "page operation cannot own captured revision trees",
                ));
            }
            Some(
                operation @ (IndexedWriteOperation::SourceCapture { .. }
                | IndexedWriteOperation::SourceCaptureBatch { .. }),
            ) => {
                let captures = operation.capture_targets();
                if self.rows.owners.len() != captures.len()
                    || captures.iter().any(|capture| {
                        !self.rows.owners.iter().any(|owner| {
                            owner.key.source_component == capture.source_id.as_str()
                                && owner.key.revision_component == capture.revision_id.as_str()
                        })
                    })
                {
                    return Err(recovery(
                        "capture operation differs from its exact manifest-derived revision owners",
                    ));
                }
            }
            _ => {}
        }
        let after: BTreeMap<_, _> = proof
            .after
            .iter()
            .map(|dep| (&dep.path, &dep.expected))
            .collect();
        if self.version == 3 {
            require_selected_delta_scope(&self.rows, &after)?;
        }
        if self
            .rows
            .dependencies
            .iter()
            .any(|dep| after.get(&dep.path).copied() != Some(&dep.expected))
        {
            return Err(recovery(
                "refresh catalog dependency is outside the selected after state",
            ));
        }
        Ok(())
    }
}

fn load_delta(catalog: &Catalog, proof: &IndexedRefreshProof) -> Result<RetainedDelta> {
    let bytes = read_bounded(&catalog.fs, &delta_path(&proof.change)?, MAX_DELTA_BYTES)?
        .ok_or_else(|| recovery("retained refresh delta is missing"))?;
    if Blake3Hash::digest(&bytes) != proof.delta_hash {
        return Err(recovery("retained refresh delta hash changed"));
    }
    strict_json(&bytes)
}

impl<'a> IndexedRefreshSession<'a> {
    /// Prepare and retain only a complete semantic projection. The private
    /// capability cannot be constructed from caller-supplied row actions.
    pub(crate) fn prepare_projected(
        catalog: &Catalog,
        writer: &'a WriterPermit,
        projected: super::source_projection::ProjectedSourceRefresh,
    ) -> Result<Self> {
        let parts = projected.into_parts();
        Self::prepare_parts(
            catalog,
            writer,
            super::write_projection::ProjectedWriteParts {
                operation: IndexedWriteOperation::SourceRefresh {
                    source_id: parts.source_id,
                },
                draft: parts.draft,
                base: parts.base,
                before: parts.before,
                after: parts.after,
                delta: parts.delta,
            },
        )
    }

    pub(crate) fn prepare_write(
        catalog: &Catalog,
        writer: &'a WriterPermit,
        projected: super::write_projection::ProjectedWrite,
    ) -> Result<Self> {
        Self::prepare_parts(catalog, writer, projected.into_parts())
    }

    pub(crate) fn seal_named_write(
        catalog: &Catalog,
        writer: &WriterPermit,
        projected: super::write_projection::ProjectedWrite,
        identity: crate::changes::types::NamedChangeIdentity,
    ) -> Result<SealedIndexedPreparation> {
        let mut parts = projected.into_parts();
        if !matches!(
            &parts.operation,
            IndexedWriteOperation::SourceCaptureBatch { .. }
                | IndexedWriteOperation::JobBatch { .. }
        ) {
            return Err(recovery(
                "named preparation requires admitted capture or exact JobBatch",
            ));
        }
        let engine = Self::admitted_engine(catalog, writer, &parts)?;
        let change = if matches!(&parts.operation, IndexedWriteOperation::JobBatch { .. }) {
            engine.seal_named_job(writer, identity, &parts.operation, parts.draft)?
        } else {
            engine.seal_named(writer, identity, parts.draft)?
        };
        parts.delta.owners = engine.sealed_revision_owners(&change)?;
        parts.delta.validate()?;
        let delta = RetainedDelta {
            version: DELTA_VERSION,
            vault_id: catalog.vault_id.clone(),
            source_id: None,
            operation: Some(parts.operation),
            change: change.prepared(),
            base: parts.base,
            before: parts.before,
            after: parts.after,
            rows: parts.delta,
        };
        super::normalized_delta::counted(&delta, MAX_DELTA_BYTES)?;
        let bytes =
            serde_json::to_vec(&delta).map_err(|error| WikiError::invalid(error.to_string()))?;
        if bytes.len() > MAX_DELTA_BYTES {
            return Err(budget("named refresh delta exceeds byte ceiling"));
        }
        let delta_hash = Blake3Hash::digest(&bytes);
        let proof = IndexedRefreshProof {
            version: 3,
            vault_id: delta.vault_id.clone(),
            source_id: None,
            operation: delta.operation.clone(),
            change: delta.change.clone(),
            base: delta.base.clone(),
            intended: intended(&delta.base, &delta.change, &delta_hash, DELTA_VERSION)?,
            delta_hash,
            before: delta.before.clone(),
            after: delta.after.clone(),
        };
        proof.validate_manifest(change.manifest())?;
        let intent = NamedIndexedIntent {
            manifest: change.manifest().clone(),
            proof,
        };
        Ok(SealedIndexedPreparation {
            engine,
            change,
            delta,
            intent,
        })
    }

    /// The caller has durably stored this exact intent before entering retention.
    /// Restart dispatch must reconcile active and terminal attempts first.
    pub(crate) fn prepare_named_write(
        catalog: &Catalog,
        writer: &'a WriterPermit,
        expected: &NamedIndexedIntent,
        sealed: SealedIndexedPreparation,
    ) -> Result<Self> {
        if expected != sealed.intent() {
            return Err(recovery(
                "named indexed admission differs from frozen import intent",
            ));
        }
        let retained_change = expected.proof.change.clone();
        let result = (|| {
            sealed
                .engine
                .prepare_named(writer, &expected.manifest, &sealed.change)?;
            let RetainedDelta {
                operation,
                change,
                base,
                before,
                after,
                rows,
                ..
            } = sealed.delta;
            let session = Self::retain_bound(
                catalog,
                writer,
                operation.ok_or_else(|| recovery("named capture lacks operation descriptor"))?,
                change,
                base,
                before,
                after,
                rows,
                DELTA_VERSION,
            )?;
            if session.proof() != &expected.proof {
                return Err(recovery(
                    "retained named indexed proof differs from import intent",
                ));
            }
            sealed
                .engine
                .stage_indexed_refresh_proof(writer, session.proof())?;
            Ok(session)
        })();
        result.map_err(|error| retained_error(error, &retained_change))
    }

    fn prepare_parts(
        catalog: &Catalog,
        writer: &'a WriterPermit,
        parts: super::write_projection::ProjectedWriteParts,
    ) -> Result<Self> {
        Self::prepare_parts_with_encoded_limits(
            catalog,
            writer,
            parts,
            crate::changes::prepare::MAX_JOURNAL_BYTES,
            MAX_DELTA_BYTES,
        )
    }

    #[cfg(test)]
    pub(crate) fn prepare_batch_with_encoded_limits(
        catalog: &Catalog,
        writer: &'a WriterPermit,
        parts: super::write_projection::ProjectedWriteParts,
        proof_maximum: usize,
        delta_maximum: usize,
    ) -> Result<Self> {
        if !matches!(
            &parts.operation,
            IndexedWriteOperation::SourceRefreshBatch { .. }
        ) {
            return Err(recovery(
                "encoded admission fixture requires SourceRefreshBatch",
            ));
        }
        Self::prepare_parts_with_encoded_limits(
            catalog,
            writer,
            parts,
            proof_maximum,
            delta_maximum,
        )
    }

    fn prepare_parts_with_encoded_limits(
        catalog: &Catalog,
        writer: &'a WriterPermit,
        mut parts: super::write_projection::ProjectedWriteParts,
        proof_maximum: usize,
        delta_maximum: usize,
    ) -> Result<Self> {
        if matches!(
            &parts.operation,
            IndexedWriteOperation::SourceRefreshBatch { .. }
        ) {
            // These tuples cover all unbounded receipt/delta fields without
            // allocating their encoded bytes. Remaining proof fields are fixed
            // bounded IDs/hashes/snapshots (<16 KiB); <=16 fresh owner rows plus
            // the delta envelope fit the separate conservative 1 MiB reserve.
            super::normalized_delta::counted(
                &(&parts.operation, &parts.before, &parts.after, &parts.base),
                proof_maximum.checked_sub(16 * 1024).ok_or_else(|| {
                    budget("batch proof byte ceiling is below its fixed-field reserve")
                })?,
            )?;
            super::normalized_delta::counted(
                &(
                    &parts.operation,
                    &parts.before,
                    &parts.after,
                    &parts.base,
                    &parts.delta,
                ),
                delta_maximum.checked_sub(1024 * 1024).ok_or_else(|| {
                    budget("batch delta byte ceiling is below its fixed-field reserve")
                })?,
            )?;
        }
        let engine = Self::admitted_engine(catalog, writer, &parts)?;
        if matches!(&parts.operation, IndexedWriteOperation::JobBatch { .. }) {
            return Err(recovery("JobBatch requires exact frozen named intent"));
        }
        let change = engine.prepare(writer, parts.draft)?.prepared;
        let retained_change = change.clone();
        let result: Result<Self> = (|| {
            parts.delta.owners = engine.manifest_revision_owners(&change)?;
            let session = Self::retain_bound(
                catalog,
                writer,
                parts.operation,
                change,
                parts.base,
                parts.before,
                parts.after,
                parts.delta,
                DELTA_VERSION,
            )?;
            engine.stage_indexed_refresh_proof(writer, session.proof())?;
            Ok(session)
        })();
        result.map_err(|error| retained_error(error, &retained_change))
    }

    fn admitted_engine(
        catalog: &Catalog,
        writer: &WriterPermit,
        parts: &super::write_projection::ProjectedWriteParts,
    ) -> Result<ChangeEngine> {
        writer.require_root(catalog.fs.root())?;
        if parts.delta.version != 3 || !parts.delta.owners.is_empty() {
            return Err(recovery(
                "projected refresh has an invalid publication envelope",
            ));
        }
        for capture in parts.operation.capture_targets() {
            let root = catalog.fs.root().resolve(&VaultRelativePath::new(format!(
                "sources/{}",
                capture.source_id
            ))?)?;
            match std::fs::symlink_metadata(root) {
                Ok(_) => {
                    return Err(WikiError::new(
                        ErrorCode::ContentConflict,
                        "new source directory became occupied before preparation",
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(WikiError::new(
                        ErrorCode::Internal,
                        format!("inspect source allocation: {error}"),
                    ));
                }
            }
        }
        parts.operation.validate()?;
        parts.delta.validate()?;
        let query = catalog.query_snapshot(QueryReadLimits::default())?;
        if QueryCatalog::snapshot(&query) != &parts.base {
            return Err(recovery(
                "projected refresh base changed before preparation",
            ));
        }
        RevisionOwnershipLookup::require_ready(&query)?;
        parts
            .delta
            .require_layout(QueryCatalog::connection(&query))?;
        parts
            .delta
            .check_before_operation(QueryCatalog::connection(&query), Some(&parts.operation))?;
        drop(query);
        let paths = PublishedRefreshPaths::admitted(
            catalog.fs.root(),
            &parts.before,
            &parts.after,
            Some(&parts.operation),
        );
        ChangeEngine::new(catalog.fs.with_published_refresh_paths(paths)?)
    }

    /// Raw row actions are only a recovery-mechanics fixture constructor.
    /// Their legacy envelope is never accepted by production replay.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn retain(
        catalog: &Catalog,
        writer: &'a WriterPermit,
        source_id: RecordId,
        change: PreparedChange,
        base: ReadSnapshot,
        before: Vec<ReadDependency>,
        after: Vec<ReadDependency>,
        rows: CatalogDelta,
    ) -> Result<Self> {
        Self::retain_bound(
            catalog,
            writer,
            IndexedWriteOperation::SourceRefresh { source_id },
            change,
            base,
            before,
            after,
            rows,
            1,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn retain_bound(
        catalog: &Catalog,
        writer: &'a WriterPermit,
        operation: IndexedWriteOperation,
        change: PreparedChange,
        base: ReadSnapshot,
        before: Vec<ReadDependency>,
        after: Vec<ReadDependency>,
        rows: CatalogDelta,
        version: u32,
    ) -> Result<Self> {
        writer.require_root(catalog.fs.root())?;
        rows.validate()?;
        operation.validate()?;
        let (source_id, operation) = if version <= 2 {
            let IndexedWriteOperation::SourceRefresh { source_id } = operation else {
                return Err(recovery("legacy retained delta requires source refresh"));
            };
            (Some(source_id), None)
        } else {
            (None, Some(operation))
        };
        let delta = RetainedDelta {
            version,
            vault_id: catalog.vault_id.clone(),
            source_id,
            operation,
            change,
            base,
            before,
            after,
            rows,
        };
        super::normalized_delta::counted(&delta, MAX_DELTA_BYTES)?;
        let bytes =
            serde_json::to_vec(&delta).map_err(|error| WikiError::invalid(error.to_string()))?;
        if bytes.len() > MAX_DELTA_BYTES {
            return Err(budget("retained refresh delta exceeds byte ceiling"));
        }
        let delta_hash = Blake3Hash::digest(&bytes);
        let proof = IndexedRefreshProof {
            version: if version <= 2 { 2 } else { 3 },
            vault_id: delta.vault_id.clone(),
            source_id: delta.source_id.clone(),
            operation: delta.operation.clone(),
            change: delta.change.clone(),
            base: delta.base.clone(),
            intended: intended(&delta.base, &delta.change, &delta_hash, version)?,
            delta_hash,
            before: delta.before.clone(),
            after: delta.after.clone(),
        };
        // Check the complete envelope and selected catalog before retaining an
        // intent. Nothing here mutates canonical files or advances publication.
        let session = Self::open(catalog, writer, proof, delta)?;
        if session.phase != IndexedRefreshPhase::AtBase {
            return Err(recovery(
                "new refresh plan requires its exact starting publication",
            ));
        }
        let path = delta_path(&session.proof.change)?;
        match read_bounded(&catalog.fs, &path, MAX_DELTA_BYTES)? {
            Some(retained) if retained == bytes => {
                journal::require_sync(catalog.fs.sync_target(&path, writer)?)?;
            }
            Some(_) => {
                return Err(recovery(
                    "refresh delta already exists with different bytes",
                ));
            }
            None => {
                let stage = catalog.fs.stage(&path, &bytes, writer)?;
                journal::require_sync(catalog.fs.replace(
                    stage,
                    &ExpectedState::Absent,
                    writer,
                )?)?;
            }
        }
        session.validate_before_files()?;
        Ok(session)
    }

    pub(crate) fn resume(
        catalog: &Catalog,
        writer: &'a WriterPermit,
        proof: IndexedRefreshProof,
    ) -> Result<Self> {
        let delta = load_delta(catalog, &proof)?;
        Self::open(catalog, writer, proof, delta)
    }

    /// Read-only live replay admission for dry-run. Historical terminal outcomes
    /// must be checked first: they need neither obsolete SQL nor retained payloads.
    pub(crate) fn check_replay(
        catalog: &Catalog,
        proof: &IndexedRefreshProof,
    ) -> Result<IndexedRefreshPhase> {
        let engine = ChangeEngine::new(catalog.fs.clone())?;
        if engine.load_indexed_refresh_proof(&proof.change)?.as_ref() != Some(proof) {
            return Err(recovery(
                "refresh replay lost or changed its exact baseline",
            ));
        }
        let delta = load_delta(catalog, proof)?;
        if !matches!(delta.version, 2 | 3)
            || (delta.version == 2 && delta.rows.version != 2)
            || (delta.version == 3 && delta.rows.version != 3)
        {
            return Err(recovery(
                "read-only indexed replay requires a supported production delta version",
            ));
        }
        delta.require_bound(catalog, &engine, proof)?;
        let query = catalog.query_snapshot(QueryReadLimits::default())?;
        query.require_fact_layout()?;
        RevisionOwnershipLookup::require_ready(&query)?;
        delta
            .rows
            .require_layout(QueryCatalog::connection(&query))?;
        QueryCatalog::connection(&query).prepare(
            "SELECT record_id FROM identity_claims INDEXED BY identity_claim_paths WHERE path=?1 LIMIT 1",
        ).map_err(sql::sql_error)?;
        let phase = if QueryCatalog::snapshot(&query) == &proof.intended {
            // Query acquisition already bounded/validated the whole header.
            // Compare provenance in SQL without allocating another owned header.
            let origin: Option<bool> = QueryCatalog::connection(&query).query_row(
                "SELECT origin_change_id=?1 AND origin_manifest_hash=?2 FROM catalog_meta WHERE singleton=1",
                params![proof.change.change_id.as_str(), proof.change.manifest_hash.as_str()],
                |row| row.get(0),
            ).map_err(sql::sql_error)?;
            if origin != Some(true) {
                return Err(recovery("intended refresh publication origin changed"));
            }
            IndexedRefreshPhase::AlreadyPublished
        } else if QueryCatalog::snapshot(&query) == &proof.base {
            IndexedRefreshPhase::AtBase
        } else {
            return Err(recovery(
                "refresh catalog is neither exact base nor intended publication",
            ));
        };
        let authority = catalog
            .operation_state()?
            .ok_or_else(|| recovery("refresh authority is absent"))?;
        let (manifest, hash) = engine.load_manifest_structure(&proof.change.change_id)?;
        proof.validate_manifest(&manifest)?;
        if hash != proof.change.manifest_hash {
            return Err(recovery("refresh replay manifest changed"));
        }
        let state = journal::load_journal(&catalog.fs, &manifest, &hash)?;
        let terminal = crate::changes::outcome::terminal_report(&catalog.fs, &manifest, &hash)?;
        if phase == IndexedRefreshPhase::AtBase && terminal.is_none() {
            // Live application still needs its retained file payloads. A dry-run
            // must not approve a proposal that the canonical executor cannot read.
            engine.validate_manifest(&manifest, &proof.change.change_id)?;
        }
        if terminal.is_none()
            && state.status == Some(ChangeStatus::Indexed)
            && state
                .frames
                .iter()
                .rev()
                .find_map(|frame| match &frame.event {
                    crate::changes::ChangeEvent::Indexed { snapshot } => Some(snapshot),
                    _ => None,
                })
                != Some(&proof.intended)
        {
            return Err(recovery(
                "indexed refresh journal names another publication",
            ));
        }
        let (after_only, mixed, needs_active) = match terminal {
            Some(report)
                if report.status == ChangeStatus::Committed
                    && report.snapshot.as_ref() == Some(&proof.intended)
                    && phase == IndexedRefreshPhase::AlreadyPublished =>
            {
                (true, false, false)
            }
            Some(report)
                if report.status == ChangeStatus::Aborted
                    && phase == IndexedRefreshPhase::AtBase =>
            {
                (false, false, false)
            }
            Some(_) => {
                return Err(recovery(
                    "refresh terminal outcome differs from retained replay",
                ));
            }
            None => match (phase, state.status) {
                (IndexedRefreshPhase::AtBase, None | Some(ChangeStatus::Prepared)) => {
                    (false, false, false)
                }
                (IndexedRefreshPhase::AtBase, Some(ChangeStatus::Applying)) => (false, true, true),
                (IndexedRefreshPhase::AtBase, Some(ChangeStatus::FilesApplied)) => {
                    (true, false, true)
                }
                (
                    IndexedRefreshPhase::AlreadyPublished,
                    Some(ChangeStatus::FilesApplied | ChangeStatus::Indexed),
                ) => (true, false, true),
                _ => return Err(recovery("refresh journal lacks trustworthy replay intent")),
            },
        };
        require_authority(&authority, proof, phase, needs_active)?;
        if phase == IndexedRefreshPhase::AlreadyPublished {
            for owner in &delta.rows.owners {
                if RevisionOwnershipLookup::revision_owner(&query, &owner.key)?.as_ref()
                    != Some(&owner.change)
                {
                    return Err(recovery(
                        "published refresh owner row differs from exact change",
                    ));
                }
            }
        }
        let remaining = Cell::new(MAX_SELECTED_BYTES);
        verify_dependencies(
            catalog,
            if after_only {
                &proof.after
            } else {
                &proof.before
            },
            mixed.then_some(proof.after.as_slice()),
            &remaining,
        )?;
        query.verify_operations(catalog)?;
        Ok(phase)
    }

    fn open(
        catalog: &Catalog,
        writer: &'a WriterPermit,
        proof: IndexedRefreshProof,
        delta: RetainedDelta,
    ) -> Result<Self> {
        writer.require_root(catalog.fs.root())?;
        if catalog.options.busy_timeout_ms > 30_000 {
            return Err(recovery("refresh timeout exceeds ceiling"));
        }
        let engine = ChangeEngine::new(catalog.fs.clone())?;
        delta.require_bound(catalog, &engine, &proof)?;
        let timeout = Duration::from_millis(catalog.options.busy_timeout_ms);
        let selection = selector::delta_selection(&catalog.fs, writer, &catalog.vault_id, timeout)?;
        selector::ensure_delta_ready(&catalog.fs, writer, &selection, timeout)?;
        let sql_writer = selector::open_delta(&catalog.fs, writer, &selection, timeout)?;
        configure_delta(sql_writer.connection())?;
        // This index was added with bounded owned-claim replacement. An older
        // internally activated v3 sibling must refuse BEFORE canonical writes,
        // rather than discovering the missing access path during publication.
        sql_writer.connection().prepare(
            "SELECT record_id FROM identity_claims INDEXED BY identity_claim_paths WHERE path=?1 LIMIT 1",
        ).map_err(sql::sql_error)?;
        delta.rows.require_layout(sql_writer.connection())?;
        let header = normalized_read::header(sql_writer.connection(), &selection)?;
        // Inspect exact intended origin FIRST. Never open the obsolete base in
        // the committed-SQL recovery case.
        let phase = if header.snapshot == proof.intended
            && header.origin.as_ref().is_some_and(|origin| {
                origin.change_id == proof.change.change_id
                    && origin.manifest_hash == proof.change.manifest_hash
            }) {
            IndexedRefreshPhase::AlreadyPublished
        } else if header.snapshot == proof.base {
            IndexedRefreshPhase::AtBase
        } else {
            return Err(recovery(
                "refresh catalog is neither exact base nor intended publication",
            ));
        };
        let authority =
            operation_authority::load(&catalog.fs, &catalog.vault_id, Presence::Required)?
                .ok_or_else(|| recovery("refresh authority is absent"))?;
        require_authority(&authority, &proof, phase, false)?;
        let starting = if phase == IndexedRefreshPhase::AtBase {
            let query = catalog.query_snapshot(QueryReadLimits::default())?;
            if QueryCatalog::snapshot(&query) != &proof.base {
                return Err(recovery(
                    "refresh base changed before ownership lookup opened",
                ));
            }
            RevisionOwnershipLookup::require_ready(&query)?;
            Some(query)
        } else {
            None
        };
        let catalog = Catalog::with_options(
            catalog.fs.clone(),
            catalog.vault_id.clone(),
            catalog.options.clone(),
        );
        let session = Self {
            catalog,
            writer,
            sql_writer,
            starting,
            proof,
            delta,
            phase,
            selected_bytes_remaining: Cell::new(MAX_SELECTED_BYTES),
        };
        if phase == IndexedRefreshPhase::AlreadyPublished {
            session.verify_published()?;
        }
        Ok(session)
    }

    /// Only a successfully opened production replay may recover this scope.
    pub(crate) fn scoped_fs(&self, fs: &VaultFs) -> Result<VaultFs> {
        if fs.root() != self.catalog.fs.root() {
            return Err(recovery("refresh session belongs to another vault"));
        }
        if !matches!(self.delta.version, 2 | 3) {
            return Ok(fs.clone());
        }
        self.retained()?;
        fs.with_published_refresh_paths(PublishedRefreshPaths::admitted(
            self.catalog.fs.root(),
            &self.proof.before,
            &self.proof.after,
            self.proof.operation.as_ref(),
        ))
    }

    pub(crate) fn proof(&self) -> &IndexedRefreshProof {
        &self.proof
    }
    pub(crate) fn phase(&self) -> IndexedRefreshPhase {
        self.phase
    }
    pub(crate) fn starting_ownership_lookup(&self) -> Result<&dyn RevisionOwnershipLookup> {
        self.starting
            .as_ref()
            .map(|query| query as &dyn RevisionOwnershipLookup)
            .ok_or_else(|| recovery("published refresh has no starting ownership capability"))
    }
    fn retained(&self) -> Result<()> {
        let bytes = read_bounded(
            &self.catalog.fs,
            &delta_path(&self.proof.change)?,
            MAX_DELTA_BYTES,
        )?
        .ok_or_else(|| recovery("retained refresh delta disappeared"))?;
        if Blake3Hash::digest(&bytes) != self.proof.delta_hash {
            return Err(recovery("retained refresh delta changed"));
        }
        Ok(())
    }
    fn authority(&self, active: bool) -> Result<()> {
        self.writer.require_root(self.catalog.fs.root())?;
        let authority = operation_authority::load(
            &self.catalog.fs,
            &self.catalog.vault_id,
            Presence::Required,
        )?
        .ok_or_else(|| recovery("refresh authority disappeared"))?;
        require_authority(&authority, &self.proof, self.phase, active)
    }
    pub(crate) fn validate_before_files(&self) -> Result<()> {
        if self.phase != IndexedRefreshPhase::AtBase {
            return Err(recovery("refresh is already published"));
        }
        self.retained()?;
        self.authority(false)?;
        let header =
            normalized_read::header(self.sql_writer.connection(), self.sql_writer.selection())?;
        if header.snapshot != self.proof.base {
            return Err(recovery("refresh base publication changed"));
        }
        // Only exact operation targets may already contain their planned after
        // bytes during replay; unchanged selected facts must still match.
        self.verify_selected(false, true)
    }

    pub(crate) fn verify_selected(&self, after_only: bool, allow_mixed: bool) -> Result<()> {
        if after_only {
            verify_dependencies(
                &self.catalog,
                &self.proof.after,
                None,
                &self.selected_bytes_remaining,
            )
        } else {
            verify_dependencies(
                &self.catalog,
                &self.proof.before,
                allow_mixed.then_some(self.proof.after.as_slice()),
                &self.selected_bytes_remaining,
            )
        }
    }

    pub(crate) fn publish(&mut self, owners: &[RevisionOwnerRow]) -> Result<ReadSnapshot> {
        if self.phase != IndexedRefreshPhase::AtBase {
            return Err(recovery("refresh publication already occurred"));
        }
        self.retained()?;
        self.authority(true)?;
        let engine = ChangeEngine::new(self.catalog.fs.clone())?;
        let (manifest, hash) = engine.load_manifest_structure(&self.proof.change.change_id)?;
        self.proof.validate_manifest(&manifest)?;
        if hash != self.proof.change.manifest_hash
            || owners != self.delta.rows.owners
            || owners != engine.manifest_revision_owners(&self.proof.change)?
        {
            return Err(recovery("refresh publication owner or manifest mismatch"));
        }
        let state = journal::load_journal(&self.catalog.fs, &manifest, &hash)?;
        if !matches!(
            state.status,
            Some(ChangeStatus::FilesApplied | ChangeStatus::Indexed)
        ) {
            return Err(recovery(
                "refresh publication requires durable files-applied intent",
            ));
        }
        self.verify_selected(true, false)?;
        // Recheck complete immutable membership and indexed competing ownership
        // at the publication boundary, even if a caller supplied a matching slice.
        let guard = engine.indexed_revision_guard(
            self.writer,
            &self.proof.change,
            self.starting_ownership_lookup()?,
        )?;
        if guard.complete_owner_rows()? != owners {
            return Err(recovery("refresh owner membership changed"));
        }
        drop(guard);
        let connection = self.sql_writer.connection();
        configure_delta(connection)?;
        let transaction = Transaction::new_unchecked(connection, TransactionBehavior::Immediate)
            .map_err(sql::sql_error)?;
        let header = normalized_read::header(&transaction, self.sql_writer.selection())?;
        if header.snapshot != self.proof.base {
            return Err(recovery("refresh base changed before transaction"));
        }
        self.authority(true)?;
        self.delta
            .rows
            .apply_for_operation(&transaction, self.proof.operation.as_ref())?;
        let after_binding = self
            .proof
            .intended
            .publication()
            .expect("validated published proof");
        let base_binding = self
            .proof
            .base
            .publication()
            .expect("validated published base");
        let changed = transaction.execute(
            "UPDATE catalog_meta SET epoch=?1,publication_hash=?2,origin_change_id=?3,origin_manifest_hash=?4,audit_epoch=NULL,control_hash=NULL,dependency_hash=NULL WHERE singleton=1 AND file_id=?5 AND epoch=?6 AND publication_hash=?7 AND parser_hash=?8 AND state='complete'",
            params![sql::integer(self.proof.intended.generation)?,after_binding.publication_hash.as_str(),
                self.proof.change.change_id.as_str(),self.proof.change.manifest_hash.as_str(),base_binding.file_id,
                sql::integer(self.proof.base.generation)?,base_binding.publication_hash.as_str(),self.proof.base.parser_fingerprint.as_str()],
        ).map_err(sql::sql_error)?;
        if changed != 1 {
            return Err(recovery("refresh publication compare-and-swap failed"));
        }
        if let Some(fault) = &self.catalog.options.fault {
            fault.check(PublicationCheckpoint::AfterPointer)?;
        }
        self.verify_selected(true, false)?;
        self.authority(true)?;
        transaction.commit().map_err(sql::sql_error)?;
        // Set this before a fault can return: this session can never publish twice.
        self.phase = IndexedRefreshPhase::AlreadyPublished;
        self.starting = None;
        if let Some(fault) = &self.catalog.options.fault {
            fault.check(PublicationCheckpoint::AfterCommit)?;
        }
        let snapshot = self.verify_published()?;
        // The SQL publication is durable; only its acknowledgement remains.
        // AlreadyPublished recovery verifies/finalizes without retrying optional
        // maintenance. Busy external readers are a normal deferred outcome.
        self.sql_writer.checkpoint_wal().map_err(|error| {
            let mut error = retained_error(error, &self.proof.change);
            let details = error
                .details
                .as_object_mut()
                .expect("retained error details");
            details.insert(
                "maintenance".into(),
                serde_json::json!("wal_checkpoint_truncate"),
            );
            details.insert("publication_committed".into(), serde_json::json!(true));
            details.insert(
                "intended_snapshot".into(),
                serde_json::json!(self.proof.intended),
            );
            error
        })?;
        Ok(snapshot)
    }

    pub(crate) fn verify_published(&self) -> Result<ReadSnapshot> {
        configure_delta(self.sql_writer.connection())?;
        self.retained()?;
        self.authority(false)?;
        let header =
            normalized_read::header(self.sql_writer.connection(), self.sql_writer.selection())?;
        if header.snapshot != self.proof.intended
            || !header.origin.as_ref().is_some_and(|origin| {
                origin.change_id == self.proof.change.change_id
                    && origin.manifest_hash == self.proof.change.manifest_hash
            })
        {
            return Err(recovery("intended refresh publication or origin changed"));
        }
        let engine = ChangeEngine::new(self.catalog.fs.clone())?;
        let (manifest, hash) = engine.load_manifest_structure(&self.proof.change.change_id)?;
        self.proof.validate_manifest(&manifest)?;
        if hash != self.proof.change.manifest_hash
            || self.delta.rows.owners != engine.manifest_revision_owners(&self.proof.change)?
        {
            return Err(recovery("published refresh manifest ownership changed"));
        }
        let state = journal::load_journal(&self.catalog.fs, &manifest, &hash)?;
        if let Some(terminal) =
            crate::changes::outcome::terminal_report(&self.catalog.fs, &manifest, &hash)?
        {
            if terminal.status != ChangeStatus::Committed
                || terminal.snapshot.as_ref() != Some(&self.proof.intended)
            {
                return Err(recovery(
                    "refresh terminal receipt differs from intended publication",
                ));
            }
            // A valid retained terminal transcript remains proof even when the
            // operational journal is only a prefix. terminal_report checks that
            // exact prefix relationship; the receipt is durable authority.
        } else {
            if !matches!(
                state.status,
                Some(ChangeStatus::FilesApplied | ChangeStatus::Indexed)
            ) {
                return Err(recovery(
                    "published refresh lacks durable apply intent or terminal receipt",
                ));
            }
            if state.status == Some(ChangeStatus::Indexed)
                && state
                    .frames
                    .iter()
                    .rev()
                    .find_map(|frame| match &frame.event {
                        crate::changes::ChangeEvent::Indexed { snapshot } => Some(snapshot),
                        _ => None,
                    })
                    != Some(&self.proof.intended)
            {
                return Err(recovery(
                    "indexed refresh journal names another publication",
                ));
            }
        }
        let ready: bool = self
            .sql_writer
            .connection()
            .query_row(
                "SELECT revision_ownership_version=1 FROM catalog_meta WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .map_err(sql::sql_error)?;
        if !ready {
            return Err(recovery("published revision ownership registry is unready"));
        }
        for owner in &self.delta.rows.owners {
            let exists: bool = self.sql_writer.connection().query_row(
                "SELECT EXISTS(SELECT 1 FROM revision_tree_owners WHERE source_component=?1 AND revision_component=?2 AND change_id=?3 AND manifest_hash=?4)",
                params![owner.key.source_component,owner.key.revision_component,owner.change.change_id.as_str(),owner.change.manifest_hash.as_str()], |row| row.get(0),
            ).map_err(sql::sql_error)?;
            if !exists {
                return Err(recovery(
                    "published refresh owner row differs from exact change",
                ));
            }
        }
        self.verify_selected(true, false)?;
        Ok(header.snapshot)
    }
}

fn require_authority(
    authority: &Authority,
    proof: &IndexedRefreshProof,
    phase: IndexedRefreshPhase,
    require_active: bool,
) -> Result<()> {
    let base = publication(&proof.base)?;
    let intended = publication(&proof.intended)?;
    if let Some(active) = authority.active() {
        if active
            != &(ActiveOperation {
                change: proof.change.clone(),
                starting: base.clone(),
                intended,
            })
            || authority.publication() != &base
        {
            return Err(recovery(
                "refresh active authority differs from retained operation",
            ));
        }
    } else if require_active
        || authority.publication()
            != if phase == IndexedRefreshPhase::AtBase {
                &base
            } else {
                &intended
            }
    {
        return Err(recovery(
            "refresh acknowledged authority differs from required publication",
        ));
    }
    Ok(())
}

fn verify_dependencies(
    catalog: &Catalog,
    expected: &[ReadDependency],
    replay_after: Option<&[ReadDependency]>,
    remaining: &Cell<usize>,
) -> Result<()> {
    for (index, dep) in expected.iter().enumerate() {
        let bytes = read_bounded(
            &catalog.fs,
            &dep.path,
            MAX_PAYLOAD_BYTES.min(remaining.get()),
        )?;
        remaining.set(
            remaining
                .get()
                .checked_sub(bytes.as_ref().map_or(0, Vec::len))
                .ok_or_else(|| budget("selected refresh verification exceeds byte ceiling"))?,
        );
        let actual = bytes.map_or(ExpectedState::Absent, |bytes| {
            ExpectedState::Hash(Blake3Hash::digest(bytes))
        });
        let after = replay_after.and_then(|deps| deps.get(index));
        if actual != dep.expected && !after.is_some_and(|dep| actual == dep.expected) {
            return Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                format!("selected refresh dependency changed: {}", dep.path),
            ));
        }
    }
    Ok(())
}

pub(super) fn configure_delta(connection: &rusqlite::Connection) -> Result<()> {
    connection
        .set_limit(Limit::SQLITE_LIMIT_LENGTH, 8 * 1024 * 1024)
        .map_err(sql::sql_error)?;
    connection
        .execute_batch("PRAGMA mmap_size=0; PRAGMA cache_size=-8192; PRAGMA temp_store=FILE;")
        .map_err(sql::sql_error)?;
    let started = Instant::now();
    let mut steps = 0u64;
    connection
        .progress_handler(
            1000,
            Some(move || {
                steps += 1000;
                steps > 10_000_000 || started.elapsed() > Duration::from_secs(30)
            }),
        )
        .map_err(sql::sql_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_scope_rejects_unrelated_empty_owner_replacements() {
        use super::super::normalized_delta::{OwnedClaims, OwnedDiagnostics, OwnedLinks};
        let selected = VaultRelativePath::new("pages/selected.md").unwrap();
        let unrelated = VaultRelativePath::new("pages/unrelated.md").unwrap();
        let expected = ExpectedState::Hash(Blake3Hash::digest(b"selected canonical bytes"));
        let after = BTreeMap::from([(&selected, &expected)]);
        let empty = CatalogDelta {
            version: 3,
            records: vec![],
            documents: vec![],
            graph: vec![],
            links: vec![],
            diagnostics: vec![],
            claims: vec![],
            revisions: vec![],
            dependencies: vec![],
            owners: vec![],
            facts: None,
        };
        // Empty replacement rows still delete an owner's old SQL rows. They
        // therefore need the same selected authority as a nonempty insert.
        for kind in 0..3 {
            let mut delta = empty.clone();
            match kind {
                0 => delta.claims.push(OwnedClaims {
                    path: unrelated.clone(),
                    rows: vec![],
                }),
                1 => delta.diagnostics.push(OwnedDiagnostics {
                    path: unrelated.clone(),
                    rows: vec![],
                }),
                _ => delta.links.push(OwnedLinks {
                    path: unrelated.clone(),
                    rows: vec![],
                }),
            }
            assert!(require_selected_delta_scope(&delta, &after).is_err());
            match kind {
                0 => delta.claims[0].path = selected.clone(),
                1 => delta.diagnostics[0].path = selected.clone(),
                _ => delta.links[0].path = selected.clone(),
            }
            require_selected_delta_scope(&delta, &after).unwrap();
        }
    }
    use crate::vault::{VaultFs, VaultRoot};

    #[test]
    fn published_path_scope_is_root_bound_and_new_components_remain_portable() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        for directory in [&first, &second] {
            std::fs::write(directory.path().join("WIKI.md"), "---\nwiki_schema: '1'\nwiki_id: vault_paths\nwiki_kind: vault\ntitle: Paths\n---\n").unwrap();
        }
        let root = VaultRoot::explicit(first.path()).unwrap();
        let original = VaultFs::new(root.clone());
        let selected = VaultRelativePath::new("sources/source_selected/source.md").unwrap();
        let new_asset =
            VaultRelativePath::new("sources/source_selected/revisions/rev_new/content.md").unwrap();
        let before = vec![ReadDependency {
            path: selected.clone(),
            expected: ExpectedState::Hash(Blake3Hash::digest(b"selected")),
        }];
        let after = vec![ReadDependency {
            path: new_asset.clone(),
            expected: ExpectedState::Hash(Blake3Hash::digest(b"asset")),
        }];
        let scope = PublishedRefreshPaths::admitted(&root, &before, &after, None);
        assert!(
            VaultFs::new(VaultRoot::explicit(second.path()).unwrap())
                .with_published_refresh_paths(scope.clone())
                .is_err()
        );
        let scoped = original.with_published_refresh_paths(scope).unwrap();
        std::fs::create_dir_all(
            first
                .path()
                .join("sources/source_selected/revisions/rev_new"),
        )
        .unwrap();
        std::fs::write(first.path().join(selected.as_str()), b"selected").unwrap();
        std::fs::write(
            first
                .path()
                .join("sources/source_selected/revisions/rev_new/CONTENT.md"),
            b"collision",
        )
        .unwrap();
        assert!(
            scoped
                .validate_paths(std::slice::from_ref(&new_asset))
                .is_err()
        );
        // Even though the new directory exists now, its spelling was never a
        // published before-prefix. A replay must still discover its siblings.
        std::fs::remove_file(
            first
                .path()
                .join("sources/source_selected/revisions/rev_new/CONTENT.md"),
        )
        .unwrap();
        std::fs::rename(
            first
                .path()
                .join("sources/source_selected/revisions/rev_new"),
            first
                .path()
                .join("sources/source_selected/revisions/REV_NEW"),
        )
        .unwrap();
        assert!(
            scoped
                .validate_paths(std::slice::from_ref(&new_asset))
                .is_err()
        );
        let unrelated = VaultRelativePath::new("sources/source_other/source.md").unwrap();
        std::fs::create_dir(first.path().join("sources/SOURCE_OTHER")).unwrap();
        assert!(scoped.validate_paths(&[unrelated]).is_err());
        assert!(
            scoped
                .validate_paths(&[
                    selected.clone(),
                    VaultRelativePath::new("sources/source_selected/SOURCE.md").unwrap()
                ])
                .is_err()
        );
    }

    #[test]
    fn repeated_selected_verification_shares_one_byte_allowance() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("WIKI.md"), "---\nwiki_schema: '1'\nwiki_id: vault_budget\nwiki_kind: vault\ntitle: Budget fixture\n---\n").unwrap();
        std::fs::write(temp.path().join("selected.md"), b"four").unwrap();
        let catalog = Catalog::new(
            VaultFs::new(VaultRoot::explicit(temp.path()).unwrap()),
            RecordId::new("vault_budget").unwrap(),
        );
        let dependencies = vec![ReadDependency {
            path: VaultRelativePath::new("selected.md").unwrap(),
            expected: ExpectedState::Hash(Blake3Hash::digest(b"four")),
        }];
        let remaining = Cell::new(8);
        verify_dependencies(&catalog, &dependencies, None, &remaining).unwrap();
        assert_eq!(remaining.get(), 4);
        verify_dependencies(&catalog, &dependencies, None, &remaining).unwrap();
        assert_eq!(remaining.get(), 0);
        assert!(verify_dependencies(&catalog, &dependencies, None, &remaining).is_err());
        assert_eq!(remaining.get(), 0);
    }

    #[test]
    fn fresh_numeric_capture_path_work_ignores_unrelated_source_siblings() {
        use crate::vault::paths::profile;
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("WIKI.md"), "---\nwiki_schema: '1'\nwiki_id: vault_numeric\nwiki_kind: vault\ntitle: Numeric allocation\n---\n").unwrap();
        let root = VaultRoot::explicit(temp.path()).unwrap();
        let id = RecordId::new("000000000000000000000000000000000000001").unwrap();
        let revision = RecordId::new("revision_fixed").unwrap();
        let selected = VaultRelativePath::new(format!("sources/{id}/source.md")).unwrap();
        let before = vec![ReadDependency {
            path: selected.clone(),
            expected: ExpectedState::Absent,
        }];
        let after = vec![ReadDependency {
            path: selected.clone(),
            expected: ExpectedState::Hash(Blake3Hash::digest(b"after")),
        }];
        let operation = IndexedWriteOperation::SourceCapture {
            source_id: id.clone(),
            revision_id: revision,
        };
        let scope = PublishedRefreshPaths::admitted(&root, &before, &after, Some(&operation));
        assert!(!scope.published(&format!("sources/{id}")));
        let fs = VaultFs::new(root.clone())
            .with_published_refresh_paths(scope)
            .unwrap();
        std::fs::create_dir(root.path().join("sources")).unwrap();
        let mut measurements = Vec::new();
        for count in [1000, 10000] {
            for index in measurements.last().map_or(0, |_| 1000)..count {
                std::fs::create_dir(root.path().join(format!("sources/unrelated_{index:05}")))
                    .unwrap();
            }
            profile::begin();
            fs.validate_paths(std::slice::from_ref(&selected)).unwrap();
            let measured = profile::finish();
            assert!(
                measured
                    .enumerations
                    .iter()
                    .filter(|(key, _)| key.ends_with(":sources_root"))
                    .all(|(_, work)| work.entries == 0),
                "{measured:?}"
            );
            measurements.push(measured);
        }
        assert_eq!(
            measurements[0].enumerations.keys().collect::<Vec<_>>(),
            measurements[1].enumerations.keys().collect::<Vec<_>>()
        );
        // Exact type/containment checks still run in the scoped path.
        let occupied = root.path().join(format!("sources/{id}"));
        std::fs::write(&occupied, b"occupied").unwrap();
        assert!(fs.validate_paths(std::slice::from_ref(&selected)).is_err());
        std::fs::remove_file(&occupied).unwrap();
        std::fs::create_dir(&occupied).unwrap();
        std::fs::write(occupied.join("WIKI.md"), b"nested vault").unwrap();
        assert!(fs.validate_paths(std::slice::from_ref(&selected)).is_err());
        #[cfg(unix)]
        {
            std::fs::remove_dir_all(&occupied).unwrap();
            std::os::unix::fs::symlink(root.path().join("missing"), &occupied).unwrap();
            assert!(fs.validate_paths(std::slice::from_ref(&selected)).is_err());
        }
        // Without an exact sealed capture descriptor, the generic policy remains.
        let generic = PublishedRefreshPaths::admitted(&root, &before, &after, None);
        assert!(!generic.singleton_source(&format!("sources/{id}")));
    }
}

#[cfg(test)]
mod capture_batch_path_scope_tests {
    use super::*;
    use crate::changes::indexed_refresh::IndexedCaptureTarget;

    #[test]
    fn batch_numeric_source_scope_is_exact_and_absent_roots_are_never_published() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("WIKI.md"), "---\nwiki_schema: '1'\nwiki_id: vault_batch_scope\nwiki_kind: vault\ntitle: Batch scope\n---\n").unwrap();
        let root = VaultRoot::explicit(temp.path()).unwrap();
        let captures = (1..=2)
            .map(|index| IndexedCaptureTarget {
                source_id: RecordId::new(format!("{index:039}")).unwrap(),
                revision_id: RecordId::new(format!("revision_{index}")).unwrap(),
            })
            .collect::<Vec<_>>();
        let operation = IndexedWriteOperation::SourceCaptureBatch {
            captures: captures.clone(),
        };
        let before = captures
            .iter()
            .map(|capture| ReadDependency {
                path: VaultRelativePath::new(format!("sources/{}/source.md", capture.source_id))
                    .unwrap(),
                expected: ExpectedState::Absent,
            })
            .collect::<Vec<_>>();
        let after = before
            .iter()
            .map(|dependency| ReadDependency {
                path: dependency.path.clone(),
                expected: ExpectedState::Hash(Blake3Hash::digest(b"planned")),
            })
            .collect::<Vec<_>>();
        let scope = PublishedRefreshPaths::admitted(&root, &before, &after, Some(&operation));
        for capture in &captures {
            let prefix = format!("sources/{}", capture.source_id);
            assert!(scope.singleton_source(&prefix));
            assert!(!scope.published(&prefix));
            assert!(!scope.singleton_source(&format!("{prefix}/revisions")));
        }
        assert!(!scope.singleton_source("sources/000000000000000000000000000000000000003"));
        assert!(!scope.singleton_source("sources/source_legacy"));
        std::fs::create_dir(root.path().join("sources")).unwrap();
        std::fs::create_dir(root.path().join("sources/unrelated_legacy")).unwrap();
        let scoped = VaultFs::new(root.clone())
            .with_published_refresh_paths(scope)
            .unwrap();
        scoped
            .validate_paths(
                &before
                    .iter()
                    .map(|dependency| dependency.path.clone())
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        let wrong = tempfile::tempdir().unwrap();
        std::fs::write(wrong.path().join("WIKI.md"), b"marker").unwrap();
        let scope = PublishedRefreshPaths::admitted(&root, &before, &after, Some(&operation));
        assert!(
            VaultFs::new(VaultRoot::explicit(wrong.path()).unwrap())
                .with_published_refresh_paths(scope)
                .is_err()
        );
    }
}

#[cfg(test)]
#[path = "source_refresh_compat_tests.rs"]
mod compat_tests;

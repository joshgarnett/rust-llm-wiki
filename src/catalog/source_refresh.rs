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

/// Narrow path authority from semantic admission or its exact retained replay.
/// Existing prefixes are frozen from before-images, never inferred from live
/// existence: partially applied new allocations still need collision discovery.
#[derive(Clone)]
pub(crate) struct PublishedRefreshPaths {
    root: VaultRoot,
    allowed: BTreeSet<String>,
    published: BTreeSet<String>,
}
impl PublishedRefreshPaths {
    fn admitted(root: &VaultRoot, before: &[ReadDependency], after: &[ReadDependency]) -> Self {
        fn prefixes(path: &VaultRelativePath) -> impl Iterator<Item = String> + '_ {
            path.as_str()
                .match_indices('/')
                .map(|(end, _)| path.as_str()[..end].to_owned())
                .chain(std::iter::once(path.as_str().to_owned()))
        }
        Self {
            root: root.clone(),
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
fn delta_path(change: &PreparedChange) -> Result<VaultRelativePath> {
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
        if matches!(after.get(path), Some(ExpectedState::Hash(_))) {
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
    fn require_operation_rows(&self, manifest: &crate::changes::ChangeManifest) -> Result<()> {
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
            IndexedWriteOperation::PageBatch { pages } => {
                for page in pages {
                    if written(&page.id, RecordKind::Page)?.path != page.path {
                        return Err(recovery("page operation identity is bound to another path"));
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
            IndexedWriteOperation::SourceCapture {
                source_id,
                revision_id,
            } => {
                let source = written(source_id, RecordKind::Source)?;
                let revision = written(revision_id, RecordKind::Revision)?;
                let source_path = format!("sources/{source_id}/source.md");
                let root = format!("sources/{source_id}/revisions/{revision_id}");
                let required = [
                    source_path.clone(),
                    format!("{root}/revision.md"),
                    format!("{root}/original.bin"),
                ];
                let content = format!("{root}/content.md");
                if source.path.as_str() != source_path
                    || revision.path.as_str() != required[1]
                    || source.record.string("wiki_current_revision") != Some(revision_id.as_str())
                    || revision.record.string("wiki_source_id") != Some(source_id.as_str())
                    || super::scan::list(&source.record, "wiki_revisions")
                        != [revision_id.to_string()]
                    || required.iter().any(|path| {
                        !manifest
                            .operations
                            .iter()
                            .any(|op| op.target.as_str() == path)
                    })
                    || manifest.operations.iter().any(|op| {
                        op.before != ExpectedState::Absent
                            || matches!(op.after, ExpectedState::Absent)
                            || !required.iter().any(|path| op.target.as_str() == path)
                                && op.target.as_str() != content
                    })
                {
                    return Err(recovery(
                        "source capture operation is not one fresh captured revision",
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
        self.require_operation_rows(&manifest)?;
        if self.rows.owners != engine.manifest_revision_owners(&proof.change)? {
            return Err(recovery(
                "refresh delta owners differ from exact manifest roots",
            ));
        }
        match &self.operation {
            Some(IndexedWriteOperation::PageBatch { .. }) if !self.rows.owners.is_empty() => {
                return Err(recovery(
                    "page operation cannot own captured revision trees",
                ));
            }
            Some(IndexedWriteOperation::SourceCapture {
                source_id,
                revision_id,
            }) => {
                if self.rows.owners.len() != 1
                    || self.rows.owners[0].key.source_component != source_id.as_str()
                    || self.rows.owners[0].key.revision_component != revision_id.as_str()
                {
                    return Err(recovery(
                        "capture operation differs from its manifest-derived revision owner",
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

    fn prepare_parts(
        catalog: &Catalog,
        writer: &'a WriterPermit,
        mut parts: super::write_projection::ProjectedWriteParts,
    ) -> Result<Self> {
        writer.require_root(catalog.fs.root())?;
        if parts.delta.version != 3 || !parts.delta.owners.is_empty() {
            return Err(recovery(
                "projected refresh has an invalid publication envelope",
            ));
        }
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
        drop(query);
        let paths = PublishedRefreshPaths::admitted(catalog.fs.root(), &parts.before, &parts.after);
        let engine = ChangeEngine::new(catalog.fs.with_published_refresh_paths(paths)?)?;
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
        self.delta.rows.apply(&transaction)?;
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
        self.verify_published()
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

fn configure_delta(connection: &rusqlite::Connection) -> Result<()> {
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
        let scope = PublishedRefreshPaths::admitted(&root, &before, &after);
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
}

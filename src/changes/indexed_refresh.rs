//! Bounded source-refresh application over an exact retained publication plan.
//! Canonical mutation uses the same executor as whole-vault changes. Recovery
//! recognizes an already committed SQL epoch before requesting the old snapshot.
use super::{
    journal,
    operation_authority::{self as operations, Authority, Presence, Publication},
    outcome,
    prepare::{MAX_JOURNAL_BYTES, MAX_OPS, MAX_PAYLOAD_BYTES, read_bounded, strict_json},
    types::*,
};
use crate::{
    catalog::source_refresh::IndexedRefreshSession,
    domain::{Blake3Hash, ErrorCode, ReadSnapshot, RecordId, Result, VaultRelativePath, WikiError},
    vault::{ExpectedState, WriterPermit},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[path = "legacy_terminal_view.rs"]
mod legacy_terminal_view;

/// New write receipts identify their admitted operation without inventing a
/// source identity for authored pages. Version-two receipts keep their exact
/// original fields and checksum encoding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum IndexedWriteOperation {
    SourceRefresh {
        source_id: RecordId,
    },
    SourceRefreshBatch {
        refreshes: Vec<IndexedRefreshTarget>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        dependent_sources: Vec<IndexedRefreshSourceDependency>,
    },
    SourceWithdraw {
        source_id: RecordId,
    },
    SourceCapture {
        source_id: RecordId,
        revision_id: RecordId,
    },
    SourceCaptureBatch {
        captures: Vec<IndexedCaptureTarget>,
    },
    PageBatch {
        pages: Vec<IndexedPageTarget>,
    },
    JobBatch {
        run_id: RecordId,
        records: Vec<IndexedJobTarget>,
        checkpoint: Option<IndexedCheckpointTarget>,
    },
    PageRename {
        page_id: RecordId,
        from: VaultRelativePath,
        to: VaultRelativePath,
        from_hash: Blake3Hash,
        rewritten_paths: Vec<VaultRelativePath>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IndexedPageTarget {
    pub path: VaultRelativePath,
    pub id: RecordId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IndexedJobTarget {
    pub path: VaultRelativePath,
    pub id: RecordId,
    pub kind: crate::domain::RecordKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IndexedCheckpointTarget {
    pub path: VaultRelativePath,
    pub hash: Blake3Hash,
    pub run_hash: Blake3Hash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IndexedCaptureTarget {
    pub source_id: RecordId,
    pub revision_id: RecordId,
}

/// Includes unchanged members so their guards remain part of a mixed request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IndexedRefreshTarget {
    pub source_id: RecordId,
    pub previous_revision_id: RecordId,
    pub revision_id: RecordId,
    pub reused: bool,
    pub no_op: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unchanged_source: Option<String>,
}

/// Exact unchanged canonical bytes for Source rows whose eligibility changes
/// through the joint dependency closure. They are not refreshed members.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IndexedRefreshSourceDependency {
    pub source_id: RecordId,
    pub source_bytes: String,
}

impl IndexedWriteOperation {
    pub(crate) fn validate(&self) -> Result<()> {
        match self {
            Self::JobBatch {
                run_id,
                records,
                checkpoint,
            } => {
                use crate::domain::RecordKind;
                let ids: std::collections::BTreeSet<_> = records.iter().map(|r| &r.id).collect();
                if records.is_empty()
                    || records.len() > 2
                    || ids.len() != records.len()
                    || !records.windows(2).all(|p| p[0].path < p[1].path)
                    || records.iter().any(|r| match r.kind {
                        RecordKind::Run => {
                            r.id != *run_id || r.path.as_str() != format!("runs/{run_id}/run.md")
                        }
                        RecordKind::RunEvent => {
                            r.id == *run_id
                                || r.path.as_str() != format!("runs/{run_id}/events/{}.md", r.id)
                        }
                        _ => true,
                    })
                    || checkpoint.as_ref().is_some_and(|c| {
                        c.path.as_str()
                            != format!("runs/{run_id}/checkpoints/{}.json", c.run_hash.hex())
                            || !records.iter().any(|r| r.kind == RecordKind::Run)
                    })
                {
                    return Err(recovery(
                        "job operation crosses its exact Run/event/checkpoint identity boundary",
                    ));
                }
                Ok(())
            }
            Self::SourceRefresh { .. } | Self::SourceWithdraw { .. } => Ok(()),
            Self::SourceRefreshBatch {
                refreshes,
                dependent_sources,
            } => {
                let mut revision_owners = BTreeMap::new();
                let mut sources: std::collections::BTreeSet<_> =
                    refreshes.iter().map(|r| &r.source_id).collect();
                for dependency in dependent_sources {
                    if !sources.insert(&dependency.source_id)
                        || dependency.source_bytes.len() > MAX_PAYLOAD_BYTES
                    {
                        return Err(recovery(
                            "refresh batch dependent Source witnesses repeat a Source or exceed its byte ceiling",
                        ));
                    }
                }
                if refreshes.is_empty()
                    || refreshes.len() > 16
                    || refreshes.iter().all(|r| r.no_op)
                    || dependent_sources.len() > 4096
                    || !dependent_sources
                        .windows(2)
                        .all(|p| p[0].source_id < p[1].source_id)
                    || !refreshes
                        .windows(2)
                        .all(|p| p[0].source_id < p[1].source_id)
                    || refreshes.iter().any(|r| {
                        (r.no_op && (!r.reused || r.previous_revision_id != r.revision_id))
                            || r.no_op != r.unchanged_source.is_some()
                            || r.unchanged_source
                                .as_ref()
                                .is_some_and(|bytes| bytes.len() > MAX_PAYLOAD_BYTES)
                            || (!r.reused && r.previous_revision_id == r.revision_id)
                            || sources.contains(&r.previous_revision_id)
                            || sources.contains(&r.revision_id)
                            || [&r.previous_revision_id, &r.revision_id]
                                .into_iter()
                                .any(|id| {
                                    revision_owners
                                        .insert(id.clone(), r.source_id.clone())
                                        .is_some_and(|prior| prior != r.source_id)
                                })
                    })
                {
                    return Err(recovery(
                        "refresh batch requires 1–16 sorted distinct guarded existing Sources with at least one change",
                    ));
                }
                Ok(())
            }
            Self::SourceCapture {
                source_id,
                revision_id,
            } if source_id != revision_id => Ok(()),
            Self::SourceCapture { .. } => Err(recovery("source capture identities overlap")),
            Self::SourceCaptureBatch { captures } => {
                let ids: std::collections::BTreeSet<_> = captures
                    .iter()
                    .flat_map(|capture| [&capture.source_id, &capture.revision_id])
                    .collect();
                if captures.is_empty()
                    || captures.len() > 8
                    || ids.len() != captures.len() * 2
                    || !captures
                        .windows(2)
                        .all(|pair| pair[0].source_id < pair[1].source_id)
                {
                    return Err(recovery(
                        "capture batch requires 1–8 sorted, distinct source and revision pairs",
                    ));
                }
                Ok(())
            }
            Self::PageRename {
                from,
                to,
                rewritten_paths,
                ..
            } => {
                if from == to
                    || !crate::sources::revision::canonical_path(from)
                    || !crate::sources::revision::canonical_path(to)
                    || super::immutable::tree_path(from)?.is_some()
                    || super::immutable::tree_path(to)?.is_some()
                    || rewritten_paths.len().saturating_add(2) > MAX_OPS
                    || !rewritten_paths.windows(2).all(|p| p[0] < p[1])
                    || rewritten_paths.iter().any(|p| {
                        p == from || p == to || !crate::sources::revision::canonical_path(p)
                    })
                {
                    return Err(recovery(
                        "Page rename requires one distinct guarded move and sorted rewrite owners",
                    ));
                }
                Ok(())
            }
            Self::PageBatch { pages } => {
                let ids: std::collections::BTreeSet<_> =
                    pages.iter().map(|page| &page.id).collect();
                if pages.is_empty()
                    || pages.len() > 16
                    || ids.len() != pages.len()
                    || !pages.windows(2).all(|pair| pair[0].path < pair[1].path)
                    || pages
                        .iter()
                        .any(|page| !crate::sources::revision::canonical_path(&page.path))
                {
                    return Err(recovery(
                        "page operation requires 1–16 unique sorted canonical targets",
                    ));
                }
                Ok(())
            }
        }
    }

    pub(crate) fn capture_targets(&self) -> Vec<IndexedCaptureTarget> {
        match self {
            Self::SourceCapture {
                source_id,
                revision_id,
            } => vec![IndexedCaptureTarget {
                source_id: source_id.clone(),
                revision_id: revision_id.clone(),
            }],
            Self::SourceCaptureBatch { captures } => captures.clone(),
            _ => Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IndexedRefreshPhase {
    AtBase,
    AlreadyPublished,
}

/// Version two is deliberately distinct from whole-vault validation receipts.
/// The delta hash binds the versioned replay payload retained by the session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IndexedRefreshProof {
    pub version: u32,
    pub vault_id: RecordId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<RecordId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<IndexedWriteOperation>,
    pub change: PreparedChange,
    pub base: ReadSnapshot,
    pub intended: ReadSnapshot,
    pub delta_hash: Blake3Hash,
    pub before: Vec<ReadDependency>,
    pub after: Vec<ReadDependency>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    proof: IndexedRefreshProof,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    embedded_delta: Option<crate::catalog::source_refresh::RetainedDelta>,
    checksum: Blake3Hash,
}
/// Supplemental first admission for an unchanged legacy Prepared Page inverse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LegacyPageAdmission {
    pub parent: PreparedChange,
    pub prepared_journal_hash: Blake3Hash,
}
pub(crate) const MAX_LEGACY_PAGE_ENVELOPE_BYTES: usize = 1024 * 1024;
fn recovery(message: &str) -> WikiError {
    super::apply::recovery_error(message)
}
pub(crate) fn baseline_path(change: &PreparedChange) -> Result<VaultRelativePath> {
    VaultRelativePath::new(format!("changes/{}/validation.json", change.change_id))
}
fn publication(snapshot: &ReadSnapshot) -> Result<Publication> {
    let binding = snapshot
        .publication()
        .ok_or_else(|| recovery("indexed refresh requires PublishedEpoch bindings"))?;
    if binding.version != 1
        || binding.file_id.len() != 32
        || !binding
            .file_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || snapshot.generation == 0
        || snapshot.generation > i64::MAX as u64
    {
        return Err(recovery("indexed refresh publication binding is invalid"));
    }
    Ok(Publication {
        file_id: binding.file_id.clone(),
        epoch: snapshot.generation,
    })
}
impl IndexedRefreshProof {
    fn validate(&self, vault: &RecordId, change: &PreparedChange) -> Result<()> {
        match (self.version, &self.source_id, &self.operation) {
            (2, Some(_), None) => {}
            (3, None, Some(operation)) => operation.validate()?,
            (4, None, Some(operation @ IndexedWriteOperation::PageBatch { pages }))
                if pages.len() == 1 =>
            {
                operation.validate()?
            }
            _ => {
                return Err(recovery(
                    "indexed write operation identity or version differs",
                ));
            }
        }
        if &self.vault_id != vault || &self.change != change {
            return Err(recovery(
                "indexed refresh baseline identity or version differs",
            ));
        }
        let base = publication(&self.base)?;
        let intended = publication(&self.intended)?;
        if base.file_id != intended.file_id
            || base.epoch.checked_add(1) != Some(intended.epoch)
            || self.base.parser_fingerprint != self.intended.parser_fingerprint
            || self.base.publication().unwrap().publication_hash
                == self.intended.publication().unwrap().publication_hash
        {
            return Err(recovery(
                "indexed refresh baseline does not name one successor publication",
            ));
        }
        if self.before.len() > MAX_OPS
            || self.after.len() > MAX_OPS
            || self.before.len() != self.after.len()
        {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "indexed refresh dependency scope exceeds its bound",
            ));
        }
        if !self
            .before
            .windows(2)
            .all(|pair| pair[0].path < pair[1].path)
            || !self
                .after
                .windows(2)
                .all(|pair| pair[0].path < pair[1].path)
            || self
                .before
                .iter()
                .zip(&self.after)
                .any(|(before, after)| before.path != after.path)
        {
            return Err(recovery(
                "indexed refresh dependency paths are not one sorted unique scope",
            ));
        }
        Ok(())
    }
    pub(crate) fn validate_manifest(&self, manifest: &ChangeManifest) -> Result<()> {
        self.validate(&manifest.vault_id, &self.change)?;
        if self.version == 4
            && (manifest.inverse_of.is_none()
                || manifest.origin.is_some()
                || !manifest.allocated_ids.is_empty()
                || !manifest.read_preconditions.is_empty()
                || manifest.operations.len() != 1
                || manifest.operations.iter().any(|op| {
                    op.role != OperationRole::MutableRecord
                        || !op.apply_after.is_empty()
                        || !matches!(op.before, ExpectedState::Hash(_))
                        || !matches!(op.after, ExpectedState::Hash(_))
                }))
        {
            return Err(recovery(
                "legacy Page envelope requires one unchanged inverse replacement",
            ));
        }
        if self.version == 4 {
            super::prepare::validate_retained_targets(
                &self
                    .before
                    .iter()
                    .map(|dependency| dependency.path.clone())
                    .collect::<Vec<_>>(),
            )?;
        }
        if let Some(IndexedWriteOperation::JobBatch {
            records,
            checkpoint,
            ..
        }) = &self.operation
        {
            let written_asset = checkpoint
                .as_ref()
                .and_then(|c| manifest.operations.iter().find(|op| op.target == c.path));
            if manifest.operations.len() != records.len() + usize::from(written_asset.is_some())
                || records.iter().any(|r| {
                    !manifest.operations.iter().any(|op| {
                        op.target == r.path
                            && matches!(op.after, ExpectedState::Hash(_))
                            && (r.kind != crate::domain::RecordKind::RunEvent
                                || op.before == ExpectedState::Absent)
                    })
                })
            {
                return Err(recovery(
                    "job manifest has mixed targets, deletion or mutable event",
                ));
            }
            if let Some(c) = checkpoint {
                let run = manifest
                    .operations
                    .iter()
                    .find(|op| {
                        records.iter().any(|r| {
                            r.kind == crate::domain::RecordKind::Run && r.path == op.target
                        })
                    })
                    .ok_or_else(|| recovery("compact checkpoint has no guarded Run"))?;
                if !matches!(run.before, ExpectedState::Hash(_))
                    || run.after != ExpectedState::Hash(c.run_hash.clone())
                    || written_asset.is_some_and(|op| {
                        op.before != ExpectedState::Absent
                            || op.after != ExpectedState::Hash(c.hash.clone())
                            || !op.apply_after.is_empty()
                    })
                    || (written_asset.is_some()
                        && run.apply_after
                            != vec![
                                manifest
                                    .operations
                                    .iter()
                                    .position(|op| op.target == c.path)
                                    .expect("written asset"),
                            ])
                    || (written_asset.is_none()
                        && (!run.apply_after.is_empty()
                            || !self.before.iter().any(|dep| {
                                dep.path == c.path
                                    && dep.expected == ExpectedState::Hash(c.hash.clone())
                            })))
                {
                    return Err(recovery(
                        "compact asset must precede its exact guarded Run after-image",
                    ));
                }
            } else if manifest
                .operations
                .iter()
                .any(|op| !op.apply_after.is_empty())
            {
                return Err(recovery("ordinary job batch has unexpected ordering"));
            }
        }
        if let Some(IndexedWriteOperation::SourceWithdraw { .. }) = &self.operation {
            if manifest.operations.len() != 1
                || !matches!(manifest.operations[0].before, ExpectedState::Hash(_))
                || !matches!(manifest.operations[0].after, ExpectedState::Hash(_))
            {
                return Err(recovery(
                    "withdrawal must replace exactly one existing Source",
                ));
            }
        }
        if let Some(IndexedWriteOperation::PageBatch { pages }) = &self.operation {
            if manifest.operations.len() != pages.len()
                || manifest.operations.iter().any(|operation| {
                    !pages.iter().any(|page| page.path == operation.target)
                        || matches!(operation.after, ExpectedState::Absent)
                })
            {
                return Err(recovery(
                    "page operation targets differ from its retained manifest",
                ));
            }
        }
        if let Some(IndexedWriteOperation::PageRename {
            from,
            to,
            from_hash,
            rewritten_paths,
            ..
        }) = &self.operation
        {
            let destination = manifest
                .operations
                .iter()
                .position(|op| &op.target == to)
                .ok_or_else(|| recovery("Page rename destination is absent from manifest"))?;
            let mut removal_after: Vec<_> = manifest
                .operations
                .iter()
                .enumerate()
                .filter_map(|(index, op)| (&op.target != from).then_some(index))
                .collect();
            removal_after.sort_unstable();
            if manifest.origin.is_some()
                || manifest.inverse_of.is_some()
                || !manifest.allocated_ids.is_empty()
                || manifest.operations.len() != rewritten_paths.len() + 2
                || manifest.operations.iter().any(|op| {
                    if op.role != OperationRole::MutableRecord {
                        return true;
                    }
                    if &op.target == to {
                        op.before != ExpectedState::Absent
                            || !matches!(op.after, ExpectedState::Hash(_))
                            || !op.apply_after.is_empty()
                    } else if &op.target == from {
                        let mut prerequisites = op.apply_after.clone();
                        prerequisites.sort_unstable();
                        op.before != ExpectedState::Hash(from_hash.clone())
                            || op.after != ExpectedState::Absent
                            || prerequisites != removal_after
                    } else {
                        !rewritten_paths.contains(&op.target)
                            || !matches!(op.before, ExpectedState::Hash(_))
                            || !matches!(op.after, ExpectedState::Hash(_))
                            || op.before == op.after
                            || op.apply_after != vec![destination]
                    }
                })
                || !manifest.operations.iter().any(|op| &op.target == from)
                || rewritten_paths
                    .iter()
                    .any(|path| !manifest.operations.iter().any(|op| &op.target == path))
            {
                return Err(recovery(
                    "Page rename manifest differs from its exact ordered move and rewrite closure",
                ));
            }
        }
        if manifest.change_id != self.change.change_id {
            return Err(recovery(
                "indexed refresh manifest differs from baseline change",
            ));
        }
        let before: BTreeMap<_, _> = self
            .before
            .iter()
            .map(|dependency| (&dependency.path, &dependency.expected))
            .collect();
        let after: BTreeMap<_, _> = self
            .after
            .iter()
            .map(|dependency| (&dependency.path, &dependency.expected))
            .collect();
        let operations: BTreeMap<_, _> = manifest
            .operations
            .iter()
            .map(|operation| (&operation.target, operation))
            .collect();
        for operation in &manifest.operations {
            if before.get(&operation.target).copied() != Some(&operation.before)
                || after.get(&operation.target).copied() != Some(&operation.after)
            {
                return Err(recovery(
                    "indexed refresh baseline omits or changes a manifest target",
                ));
            }
        }
        for dependency in &manifest.read_preconditions {
            if before.get(&dependency.path).copied() != Some(&dependency.expected)
                || after.get(&dependency.path).copied() != Some(&dependency.expected)
            {
                return Err(recovery(
                    "indexed refresh baseline omits or changes a retained read precondition",
                ));
            }
        }
        for (before, after) in self.before.iter().zip(&self.after) {
            if !operations.contains_key(&before.path) {
                if before.expected != after.expected {
                    return Err(recovery(
                        "indexed refresh baseline grants an undeclared mutation",
                    ));
                }
                if self.version != 4
                    && !manifest
                        .read_preconditions
                        .iter()
                        .any(|dependency| dependency == before)
                {
                    return Err(recovery(
                        "selected refresh boundary is absent from retained read preconditions",
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Only this module can construct the seal, after the catalog session verifies
/// exact intended hash/origin/delta and manifest-derived current owner rows.
/// The immutable module uses it solely for complete receipt/member checking.
pub(super) struct VerifiedIndexedRefreshPublication {
    proof: IndexedRefreshProof,
}
impl VerifiedIndexedRefreshPublication {
    pub(super) fn proof(&self) -> &IndexedRefreshProof {
        &self.proof
    }
    pub(super) fn require_for(&self, engine: &ChangeEngine, writer: &WriterPermit) -> Result<()> {
        writer.require_root(engine.fs.root())?;
        engine.require_binding()?;
        self.proof.validate(&engine.vault_id, &self.proof.change)?;
        let retained = engine
            .load_indexed_refresh_proof(&self.proof.change)?
            .ok_or_else(|| recovery("published refresh lost its retained baseline"))?;
        if retained != self.proof {
            return Err(recovery("published refresh baseline changed"));
        }
        let authority = required_authority(engine)?;
        require_active(&authority, &self.proof)
    }
}
fn required_authority(engine: &ChangeEngine) -> Result<Authority> {
    operations::load(&engine.fs, &engine.vault_id, Presence::Required)?
        .ok_or_else(|| recovery("indexed refresh requires operation authority"))
}
fn require_active(authority: &Authority, proof: &IndexedRefreshProof) -> Result<()> {
    let active = authority
        .active()
        .ok_or_else(|| recovery("indexed refresh has no active reservation"))?;
    if active.change != proof.change
        || active.starting != publication(&proof.base)?
        || active.intended != publication(&proof.intended)?
    {
        return Err(recovery(
            "indexed refresh differs from exact active reservation",
        ));
    }
    Ok(())
}

impl ChangeEngine {
    /// Bounded read for recovery dispatch; absence does not authorize creating a
    /// new baseline for an applying or SQL-published operation.
    pub(crate) fn load_indexed_refresh_proof(
        &self,
        change: &PreparedChange,
    ) -> Result<Option<IndexedRefreshProof>> {
        Ok(self
            .load_indexed_refresh_retention(change)?
            .map(|(proof, _)| proof))
    }

    pub(crate) fn load_indexed_refresh_retention(
        &self,
        change: &PreparedChange,
    ) -> Result<
        Option<(
            IndexedRefreshProof,
            Option<crate::catalog::source_refresh::RetainedDelta>,
        )>,
    > {
        crate::maintenance_diagnostic::observe(
            crate::maintenance_diagnostic::Phase::RetainedAuthority,
            || {
                let Some(bytes) =
                    read_bounded(&self.fs, &baseline_path(change)?, MAX_JOURNAL_BYTES)?
                else {
                    return Ok(None);
                };
                let (proof, delta) = self.decode_indexed_refresh_retention(change, &bytes)?;
                if proof.version == 4 {
                    legacy_terminal_view::validate_full_archive(self, change, &bytes, &proof)?;
                }
                if proof.version == 3
                    && crate::catalog::source_refresh::intended(
                        &proof.base,
                        change,
                        &proof.delta_hash,
                        3,
                    )? != proof.intended
                {
                    return legacy_terminal_view::resolve(self, change, &bytes, proof).map(Some);
                }
                Ok(Some((proof, delta)))
            },
        )
    }

    /// Strict nonrecursive codec shared by current receipts and the immutable
    /// full-authority archive. Terminal-view resolution happens only above.
    fn decode_indexed_refresh_retention(
        &self,
        change: &PreparedChange,
        bytes: &[u8],
    ) -> Result<(
        IndexedRefreshProof,
        Option<crate::catalog::source_refresh::RetainedDelta>,
    )> {
        crate::maintenance_diagnostic::observe(
            crate::maintenance_diagnostic::Phase::RetainedAuthority,
            || {
                let receipt: Receipt = strict_json(bytes)?;
                let encoded = if receipt.proof.version == 4 {
                    if bytes.len() > MAX_LEGACY_PAGE_ENVELOPE_BYTES
                        || receipt.embedded_delta.is_none()
                    {
                        return Err(recovery(
                            "legacy Page envelope size or embedded delta invalid",
                        ));
                    }
                    if read_bounded(
                        &self.fs,
                        &crate::catalog::source_refresh::delta_path(change)?,
                        0,
                    )?
                    .is_some()
                    {
                        return Err(recovery(
                            "legacy Page envelope has unexpected separate delta",
                        ));
                    }
                    serde_json::to_vec(&(&receipt.proof, &receipt.embedded_delta))
                } else {
                    if receipt.embedded_delta.is_some() {
                        return Err(recovery(
                            "ordinary indexed proof cannot embed a legacy Page delta",
                        ));
                    }
                    serde_json::to_vec(&receipt.proof)
                }
                .map_err(|error| WikiError::invalid(error.to_string()))?;
                if Blake3Hash::digest(encoded) != receipt.checksum {
                    return Err(recovery("indexed refresh baseline checksum differs"));
                }
                receipt.proof.validate(&self.vault_id, change)?;
                if let Some(delta) = &receipt.embedded_delta {
                    delta.validate_embedded_envelope(&receipt.proof)?;
                }
                Ok((receipt.proof, receipt.embedded_delta))
            },
        )
    }

    /// One atomic expected-absent envelope; no second delta-file migration cut.
    pub(crate) fn stage_legacy_page_envelope(
        &self,
        writer: &WriterPermit,
        proof: &IndexedRefreshProof,
        delta: &crate::catalog::source_refresh::RetainedDelta,
    ) -> Result<()> {
        writer.require_root(self.fs.root())?;
        self.require_binding()?;
        if proof.version != 4 {
            return Err(recovery("legacy Page envelope requires version4"));
        }
        let (manifest, hash) = self.load_manifest_structure(&proof.change.change_id)?;
        proof.validate_manifest(&manifest)?;
        if hash != proof.change.manifest_hash {
            return Err(recovery("legacy Page manifest changed"));
        }
        let state = journal::load_journal(&self.fs, &manifest, &hash)?;
        if state.status != Some(ChangeStatus::Prepared)
            || state.frames.len() != 1
            || state.torn_tail
            || outcome::terminal_report(&self.fs, &manifest, &hash)?.is_some()
        {
            return Err(recovery(
                "legacy Page envelope needs original never-started Prepared history",
            ));
        }
        let authority = required_authority(self)?;
        authority.require_publication(&publication(&proof.base)?)?;
        if authority.publication() != &publication(&proof.base)? {
            return Err(recovery("legacy Page admission base changed"));
        }
        crate::catalog::normalized_delta::counted(
            &(proof, Some(delta)),
            MAX_LEGACY_PAGE_ENVELOPE_BYTES,
        )?;
        let embedded_delta = Some(delta.clone());
        let checksum = Blake3Hash::digest(
            serde_json::to_vec(&(proof, &embedded_delta))
                .map_err(|e| WikiError::invalid(e.to_string()))?,
        );
        let receipt = Receipt {
            proof: proof.clone(),
            embedded_delta,
            checksum,
        };
        crate::catalog::normalized_delta::counted(&receipt, MAX_LEGACY_PAGE_ENVELOPE_BYTES)?;
        let bytes = serde_json::to_vec(&receipt).map_err(|e| WikiError::invalid(e.to_string()))?;
        if bytes.len() > MAX_LEGACY_PAGE_ENVELOPE_BYTES {
            return Err(recovery("legacy Page envelope exceeds bound"));
        }
        if read_bounded(
            &self.fs,
            &crate::catalog::source_refresh::delta_path(&proof.change)?,
            0,
        )?
        .is_some()
        {
            return Err(recovery("legacy Page admission found separate delta"));
        }
        let staged = self
            .fs
            .stage(&baseline_path(&proof.change)?, &bytes, writer)?;
        journal::require_sync(self.fs.replace(staged, &ExpectedState::Absent, writer)?)
    }
    /// Retain a staged baseline without activating a canonical operation.
    /// Successful preparation must survive a process boundary before first apply.
    pub(crate) fn stage_indexed_refresh_proof(
        &self,
        writer: &WriterPermit,
        proof: &IndexedRefreshProof,
    ) -> Result<()> {
        writer.require_root(self.fs.root())?;
        self.require_binding()?;
        let (manifest, hash) = self.load_manifest_structure(&proof.change.change_id)?;
        if hash != proof.change.manifest_hash {
            return Err(recovery("staged indexed refresh manifest binding changed"));
        }
        proof.validate_manifest(&manifest)?;
        let state = journal::load_journal(&self.fs, &manifest, &hash)?;
        if !matches!(state.status, None | Some(ChangeStatus::Prepared))
            || outcome::terminal_report(&self.fs, &manifest, &hash)?.is_some()
        {
            return Err(recovery(
                "indexed baseline staging requires an unapplied proposal",
            ));
        }
        let authority = required_authority(self)?;
        let base = publication(&proof.base)?;
        authority.require_publication(&base)?;
        if authority.publication() != &base {
            return Err(recovery(
                "staged indexed refresh differs from acknowledged base",
            ));
        }
        self.retain_indexed_refresh_proof(writer, proof, true)
    }
    fn retain_indexed_refresh_proof(
        &self,
        writer: &WriterPermit,
        proof: &IndexedRefreshProof,
        allow_create: bool,
    ) -> Result<()> {
        crate::maintenance_diagnostic::observe(
            crate::maintenance_diagnostic::Phase::RetainedAuthority,
            || {
                if let Some(retained) = self.load_indexed_refresh_proof(&proof.change)? {
                    if retained != *proof {
                        return Err(recovery(
                            "indexed refresh must resume its exact retained plan",
                        ));
                    }
                    return journal::require_sync(
                        self.fs
                            .sync_target(&baseline_path(&proof.change)?, writer)?,
                    );
                }
                if !allow_create || proof.version == 4 {
                    return Err(recovery(
                        "active indexed refresh lost its original baseline",
                    ));
                }
                let checksum = Blake3Hash::digest(
                    serde_json::to_vec(proof)
                        .map_err(|error| WikiError::invalid(error.to_string()))?,
                );
                let bytes = serde_json::to_vec(&Receipt {
                    proof: proof.clone(),
                    embedded_delta: None,
                    checksum,
                })
                .map_err(|error| WikiError::invalid(error.to_string()))?;
                if bytes.len() > MAX_JOURNAL_BYTES {
                    return Err(WikiError::new(
                        ErrorCode::BudgetExceeded,
                        "indexed refresh baseline exceeds byte ceiling",
                    ));
                }
                let staged = self
                    .fs
                    .stage(&baseline_path(&proof.change)?, &bytes, writer)?;
                journal::require_sync(self.fs.replace(staged, &ExpectedState::Absent, writer)?)
            },
        )
    }
    #[cfg(test)]
    fn verify_indexed_dependencies(
        &self,
        proof: &IndexedRefreshProof,
        manifest: &ChangeManifest,
        after_only: bool,
        allow_mixed: bool,
    ) -> Result<()> {
        for (before, after) in proof.before.iter().zip(&proof.after) {
            let actual = self.target_state(&before.path)?;
            let valid = if after_only {
                actual == after.expected
            } else {
                actual == before.expected
                    || (allow_mixed
                        && manifest
                            .operations
                            .iter()
                            .any(|operation| operation.target == before.path)
                        && actual == after.expected)
            };
            if !valid {
                return Err(WikiError::new(
                    ErrorCode::ContentConflict,
                    format!(
                        "indexed refresh selected dependency changed: {}",
                        before.path
                    ),
                ));
            }
        }
        Ok(())
    }
    fn seal_indexed_publication(
        &self,
        writer: &WriterPermit,
        session: &IndexedRefreshSession<'_>,
        proof: &IndexedRefreshProof,
    ) -> Result<VerifiedIndexedRefreshPublication> {
        crate::maintenance_diagnostic::observe(
            crate::maintenance_diagnostic::Phase::SqlPublication,
            || {
                if session.proof() != proof || session.verify_published()? != proof.intended {
                    return Err(recovery(
                        "catalog did not verify the exact intended indexed refresh",
                    ));
                }
                let seal = VerifiedIndexedRefreshPublication {
                    proof: proof.clone(),
                };
                seal.require_for(self, writer)?;
                Ok(seal)
            },
        )
    }
    fn acknowledge_indexed_refresh(
        &self,
        writer: &WriterPermit,
        manifest: &ChangeManifest,
        proof: &IndexedRefreshProof,
        report: &ApplyReport,
    ) -> Result<()> {
        crate::maintenance_diagnostic::observe(
            crate::maintenance_diagnostic::Phase::Finalize,
            || {
                if report.status != ChangeStatus::Committed
                    || report.change != proof.change
                    || report.snapshot.as_ref() != Some(&proof.intended)
                {
                    return Err(recovery(
                        "indexed refresh terminal receipt does not prove intended publication",
                    ));
                }
                outcome::sync_receipt(&self.fs, writer, &proof.change.change_id)?;
                if outcome::terminal_report(&self.fs, manifest, &proof.change.manifest_hash)?
                    .as_ref()
                    != Some(report)
                {
                    return Err(recovery(
                        "indexed refresh terminal receipt changed before acknowledgement",
                    ));
                }
                let authority = required_authority(self)?;
                require_active(&authority, proof)?;
                operations::acknowledge(
                    &self.fs,
                    writer,
                    &authority,
                    &proof.change,
                    publication(&proof.intended)?,
                )?;
                legacy_terminal_view::finalize(self, writer, &proof.change)?;
                Ok(())
            },
        )
    }

    /// Historical terminal retries must run before constructing a live session:
    /// the original base/intended epoch may have been superseded. This proves
    /// only the retained outcome, never current canonical freshness.
    pub(crate) fn indexed_refresh_terminal_outcome(
        &self,
        change: &PreparedChange,
    ) -> Result<Option<ApplyReport>> {
        crate::maintenance_diagnostic::observe(
            crate::maintenance_diagnostic::Phase::Finalize,
            || {
                self.require_binding()?;
                let Some(proof) = self.load_indexed_refresh_proof(change)? else {
                    return Ok(None);
                };
                let (manifest, hash) = self.load_manifest_structure(&change.change_id)?;
                if hash != change.manifest_hash {
                    return Err(recovery("terminal refresh manifest binding changed"));
                }
                proof.validate_manifest(&manifest)?;
                let Some(report) = outcome::terminal_report(&self.fs, &manifest, &hash)? else {
                    return Ok(None);
                };
                if report.change != *change
                    || (report.status == ChangeStatus::Committed
                        && report.snapshot.as_ref() != Some(&proof.intended))
                {
                    return Err(recovery(
                        "terminal refresh receipt differs from retained plan",
                    ));
                }
                let authority = required_authority(self)?;
                if authority
                    .active()
                    .is_some_and(|active| active.change == *change)
                {
                    return Ok(None); // exact live session must finish acknowledgement/cancel
                }
                let expected = publication(if report.status == ChangeStatus::Committed {
                    &proof.intended
                } else {
                    &proof.base
                })?;
                let floor = authority.publication();
                if floor.epoch < expected.epoch
                    || (floor.epoch == expected.epoch && floor.file_id != expected.file_id)
                {
                    return Err(recovery(
                        "historical refresh outcome exceeds acknowledged publication",
                    ));
                }
                Ok(Some(report))
            },
        )
    }

    pub(crate) fn indexed_refresh_terminal_report(
        &self,
        writer: &WriterPermit,
        change: &PreparedChange,
    ) -> Result<Option<ApplyReport>> {
        writer.require_root(self.fs.root())?;
        let Some(report) = self.indexed_refresh_terminal_outcome(change)? else {
            return Ok(None);
        };
        outcome::sync_receipt(&self.fs, writer, &change.change_id)?;
        if self.indexed_refresh_terminal_outcome(change)?.as_ref() != Some(&report) {
            return Err(recovery("historical refresh receipt changed while syncing"));
        }
        if report.status == ChangeStatus::Committed {
            legacy_terminal_view::finalize(self, writer, change)?;
        }
        Ok(Some(report))
    }

    pub(crate) fn apply_indexed_refresh(
        &self,
        writer: &WriterPermit,
        session: &mut IndexedRefreshSession<'_>,
    ) -> Result<ApplyReport> {
        let scoped = Self {
            fs: session.scoped_fs(&self.fs)?,
            vault_id: self.vault_id.clone(),
        };
        scoped.apply_indexed_refresh_scoped(writer, session)
    }

    fn apply_indexed_refresh_scoped(
        &self,
        writer: &WriterPermit,
        session: &mut IndexedRefreshSession<'_>,
    ) -> Result<ApplyReport> {
        crate::maintenance_diagnostic::observe(crate::maintenance_diagnostic::Phase::Apply, || {
            writer.require_root(self.fs.root())?;
            self.require_binding()?;
            let proof = session.proof().clone();
            proof.validate(&self.vault_id, &proof.change)?;
            let (manifest, hash) = self.load_manifest_structure(&proof.change.change_id)?;
            if hash != proof.change.manifest_hash {
                return Err(recovery("indexed refresh retained manifest changed"));
            }
            proof.validate_manifest(&manifest)?;
            let terminal = outcome::terminal_report(&self.fs, &manifest, &hash)?;
            let state = journal::load_journal(&self.fs, &manifest, &hash)?;
            let mut authority = required_authority(self)?;
            let phase = session.phase();
            let allow_create = phase == IndexedRefreshPhase::AtBase
                && authority.active().is_none()
                && terminal.is_none()
                && matches!(state.status, None | Some(ChangeStatus::Prepared));
            self.retain_indexed_refresh_proof(writer, &proof, allow_create)?;

            if let Some(report) = terminal {
                if report.change != proof.change {
                    return Err(recovery("indexed refresh terminal identity differs"));
                }
                if report.status == ChangeStatus::Aborted {
                    if phase != IndexedRefreshPhase::AtBase {
                        return Err(recovery("aborted indexed refresh has a published delta"));
                    }
                    session.verify_selected(false, false)?;
                    self.require_abandoned_revision_trees(&manifest)?;
                    outcome::sync_receipt(&self.fs, writer, &proof.change.change_id)?;
                    if authority.active().is_some() {
                        require_active(&authority, &proof)?;
                        operations::cancel(&self.fs, writer, &authority, &proof.change)?;
                    }
                    return Ok(report);
                }
                if report.snapshot.as_ref() != Some(&proof.intended) {
                    return Err(recovery(
                        "terminal indexed refresh names another publication",
                    ));
                }
                if authority.active().is_none() {
                    let committed = publication(&proof.intended)?;
                    let floor = authority.publication();
                    if floor.epoch < committed.epoch
                        || (floor.epoch == committed.epoch && floor.file_id != committed.file_id)
                    {
                        return Err(recovery(
                            "terminal indexed refresh has not been acknowledged by idle authority",
                        ));
                    }
                    outcome::sync_receipt(&self.fs, writer, &proof.change.change_id)?;
                    legacy_terminal_view::finalize(self, writer, &proof.change)?;
                    return Ok(report);
                }
                require_active(&authority, &proof)?;
                if phase != IndexedRefreshPhase::AlreadyPublished {
                    return Err(recovery(
                        "committed indexed refresh SQL publication is absent",
                    ));
                }
                session.verify_selected(true, false)?;
                self.require_all_after(writer, &manifest, &hash)?;
                let seal = self.seal_indexed_publication(writer, session, &proof)?;
                self.verify_indexed_finalization(writer, &seal)?;
                self.acknowledge_indexed_refresh(writer, &manifest, &proof, &report)?;
                return Ok(report);
            }
            if matches!(
                state.status,
                Some(ChangeStatus::Conflict | ChangeStatus::Aborted)
            ) {
                return Err(recovery(
                    "indexed refresh has a durable conflict or aborted intent",
                ));
            }
            // A known unapplied capture cannot adopt an independently occupied
            // Source parent, even when its planned revision tree is still absent.
            // Applying recovery uses the retained tree/member and journal authority.
            if phase == IndexedRefreshPhase::AtBase
                && matches!(state.status, None | Some(ChangeStatus::Prepared))
                && let Some(operation) = &proof.operation
            {
                for capture in operation.capture_targets() {
                    let path = self.fs.root().resolve(&VaultRelativePath::new(format!(
                        "sources/{}",
                        capture.source_id
                    ))?)?;
                    match std::fs::symlink_metadata(path) {
                        Ok(_) => {
                            return Err(WikiError::new(
                                ErrorCode::ContentConflict,
                                "source namespace was independently occupied after preparation",
                            ));
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => {
                            return Err(WikiError::new(
                                ErrorCode::Internal,
                                format!("inspect prepared source namespace: {error}"),
                            ));
                        }
                    }
                }
            }
            if phase == IndexedRefreshPhase::AlreadyPublished {
                require_active(&authority, &proof)?;
                if !matches!(
                    state.status,
                    Some(
                        ChangeStatus::FilesApplied
                            | ChangeStatus::Indexed
                            | ChangeStatus::Committed
                    )
                ) {
                    return Err(recovery(
                        "published indexed refresh lacks surviving files-applied intent",
                    ));
                }
                // Never replay canonical writes or try to open the vanished base.
                session.verify_selected(true, false)?;
                self.require_all_after(writer, &manifest, &hash)?;
            } else {
                if !matches!(
                    state.status,
                    None | Some(
                        ChangeStatus::Prepared
                            | ChangeStatus::Applying
                            | ChangeStatus::FilesApplied
                    )
                ) {
                    return Err(recovery(
                        "base indexed refresh has incompatible journal publication",
                    ));
                }
                if authority.active().is_none() {
                    if authority.publication() != &publication(&proof.base)?
                        || !matches!(state.status, None | Some(ChangeStatus::Prepared))
                    {
                        return Err(recovery("indexed refresh base differs from idle authority"));
                    }
                    session.verify_selected(false, false)?;
                    session.validate_before_files()?;
                    authority = operations::begin(
                        &self.fs,
                        writer,
                        &authority,
                        proof.change.clone(),
                        publication(&proof.intended)?,
                    )?;
                }
                require_active(&authority, &proof)?;
                let mixed = matches!(
                    state.status,
                    Some(ChangeStatus::Applying | ChangeStatus::FilesApplied)
                );
                session.verify_selected(false, mixed)?;
                session.validate_before_files()?;
                let observations = self.observe(&manifest)?;
                if observations.iter().any(|value| {
                    value.observed != value.before && (!mixed || value.observed != value.after)
                }) {
                    return self.conflict(
                        writer,
                        &manifest,
                        &hash,
                        "indexed refresh preflight",
                        observations,
                    );
                }
                // Verify retained payloads before the shared executor can mutate.
                self.validate_manifest(&manifest, &proof.change.change_id)?;
                let guard = self.indexed_revision_guard(
                    writer,
                    &proof.change,
                    session.starting_ownership_lookup()?,
                )?;
                if let Err(error) = guard.preflight() {
                    return self.revision_failure(writer, &manifest, &hash, &state, error);
                }
                self.apply_files_to_files_applied(
                    writer,
                    &proof.change,
                    &manifest,
                    &state,
                    &observations,
                    &|complete| {
                        if let Err(error) = guard.verify(complete) {
                            let state = journal::load_journal(&self.fs, &manifest, &hash)?;
                            return self.revision_failure(writer, &manifest, &hash, &state, error);
                        }
                        Ok(())
                    },
                )?;
                self.require_all_after(writer, &manifest, &hash)?;
                session.verify_selected(true, false)?;
                let owners = guard.complete_owner_rows()?;
                drop(guard);
                if session.publish(&owners)? != proof.intended {
                    return Err(recovery("indexed refresh published an unexpected snapshot"));
                }
            }
            session.verify_selected(true, false)?;
            self.require_all_after(writer, &manifest, &hash)?;
            let seal = self.seal_indexed_publication(writer, session, &proof)?;
            self.verify_indexed_finalization(writer, &seal)?;
            let state = journal::load_journal(&self.fs, &manifest, &hash)?;
            let report = match state.status {
                Some(ChangeStatus::Committed) => {
                    outcome::retain_terminal(self, writer, &manifest, &hash, &state)?
                }
                Some(ChangeStatus::FilesApplied) => {
                    journal::append_event(
                        &self.fs,
                        writer,
                        &manifest,
                        &hash,
                        ChangeEvent::Indexed {
                            snapshot: proof.intended.clone(),
                        },
                    )?;
                    outcome::finish(self, writer, &manifest, &hash, ChangeStatus::Committed)?
                }
                Some(ChangeStatus::Indexed) => {
                    let indexed = state
                        .frames
                        .iter()
                        .rev()
                        .find_map(|frame| match &frame.event {
                            ChangeEvent::Indexed { snapshot } => Some(snapshot),
                            _ => None,
                        });
                    if indexed != Some(&proof.intended) {
                        return Err(recovery(
                            "indexed journal snapshot differs from retained intended publication",
                        ));
                    }
                    outcome::finish(self, writer, &manifest, &hash, ChangeStatus::Committed)?
                }
                _ => {
                    return Err(recovery(
                        "indexed refresh cannot finalize from its journal state",
                    ));
                }
            };
            self.acknowledge_indexed_refresh(writer, &manifest, &proof, &report)?;
            Ok(report)
        })
    }
}

#[cfg(test)]
#[path = "indexed_refresh_tests.rs"]
mod tests;

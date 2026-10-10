//! Abort only unapplied proposals; rollback is a separately retained, validated inverse.
use super::{apply::recovery_error, journal, outcome, types::*};
use crate::{
    domain::{Blake3Hash, ErrorCode, RecordKind, Result, WikiError},
    records::parse_note,
    vault::{ExpectedState, WriterPermit},
};
use std::collections::{BTreeMap, BTreeSet};
/// Read-only authority for newly projected, authenticated Page replacements.
/// The draft is private so ordinary drafts cannot claim this admission.
pub(crate) struct ValidatedPageInverse {
    draft: ChangeDraft,
}
impl ValidatedPageInverse {
    pub(crate) fn draft(&self) -> &ChangeDraft {
        &self.draft
    }
    pub(crate) fn into_draft(self) -> ChangeDraft {
        self.draft
    }
}

const PAGE_INVERSE_BYTES: usize = 16 * 1024 * 1024;

impl ChangeEngine {
    pub fn abort(&self, permit: &WriterPermit, change: &PreparedChange) -> Result<ApplyReport> {
        permit.require_root(self.fs.root())?;
        self.require_binding()?;
        let (manifest, hash) = self.load_manifest(&change.change_id)?;
        if hash != change.manifest_hash {
            return Err(WikiError::invalid("prepared manifest binding changed"));
        }
        if let Some(report) = outcome::terminal_report(&self.fs, &manifest, &hash)? {
            if report.status == ChangeStatus::Aborted {
                outcome::sync_receipt(&self.fs, permit, &manifest.change_id)?;
                return Ok(report);
            }
            return Err(recovery_error(
                "committed change requires a guarded inverse",
            ));
        }
        let state = journal::load_journal(&self.fs, &manifest, &hash)?;
        if state.status == Some(ChangeStatus::Aborted) {
            return outcome::retain_terminal(self, permit, &manifest, &hash, &state);
        }
        if !matches!(state.status, None | Some(ChangeStatus::Prepared)) {
            return Err(recovery_error(
                "an applying or conflicted change cannot be aborted",
            ));
        }
        if state.status.is_none() {
            let observations = self.observe(&manifest)?;
            if observations.iter().any(|o| o.observed != o.before) {
                return Err(recovery_error(
                    "abort requires all targets at their prepared old state",
                ));
            }
            journal::append_event(&self.fs, permit, &manifest, &hash, ChangeEvent::Prepared)?;
        }
        outcome::finish(self, permit, &manifest, &hash, ChangeStatus::Aborted)
    }
    /// Authenticate a bounded committed Page lineage without allocating a proposal.
    /// Fresh projection adds its own read constraints to the newly retained inverse.
    pub(crate) fn page_inverse_plan(
        &self,
        change: &PreparedChange,
    ) -> Result<ValidatedPageInverse> {
        self.require_binding()?;
        let mut remaining = super::prepare::MAX_INVERSE_PAYLOAD_BYTES;
        let (mut current, hash) =
            self.load_manifest_with_budget(&change.change_id, &mut remaining)?;
        if hash != change.manifest_hash {
            return Err(WikiError::invalid("prepared manifest binding changed"));
        }
        let mut seen = BTreeSet::new();
        let mut inverse = None;
        let mut current_hash = hash.clone();
        for depth in 1..=64 {
            if !seen.insert(current.change_id.clone()) {
                return Err(WikiError::invalid("Page inverse ancestry cycle"));
            }
            let committed = match outcome::terminal_report(&self.fs, &current, &current_hash)? {
                Some(report) => report.status == ChangeStatus::Committed,
                None => {
                    journal::load_journal(&self.fs, &current, &current_hash)?.status
                        == Some(ChangeStatus::Committed)
                }
            };
            if !committed {
                return Err(WikiError::new(
                    ErrorCode::RecoveryRequired,
                    "Page inverse requires authenticated committed history",
                ));
            }
            let operations = self.page_replacement_reversal(&current, &mut remaining)?;
            if inverse.is_none() {
                for operation in &operations {
                    let bytes = super::prepare::read_bounded(
                        &self.fs,
                        &operation.target,
                        remaining.min(PAGE_INVERSE_BYTES),
                    )?
                    .ok_or_else(|| {
                        WikiError::new(
                            ErrorCode::ContentConflict,
                            "Page inverse target disappeared",
                        )
                    })?;
                    remaining = remaining
                        .checked_sub(bytes.len())
                        .ok_or_else(page_inverse_budget)?;
                    if operation.expected != ExpectedState::Hash(Blake3Hash::digest(&bytes)) {
                        return Err(WikiError::new(
                            ErrorCode::ContentConflict,
                            "Page changed before inverse admission",
                        ));
                    }
                    let actual = parse_note(&bytes);
                    let proposed =
                        parse_note(operation.proposed.as_ref().expect("replacement verified"));
                    if actual.canonical.as_ref().is_none_or(|record| {
                        record.kind() != RecordKind::Page
                            || proposed
                                .canonical
                                .as_ref()
                                .is_none_or(|before| before.id() != record.id())
                    }) {
                        return Err(WikiError::invalid(
                            "Page inverse must preserve current identity and kind",
                        ));
                    }
                }
                inverse = Some(ChangeDraft {
                    title: format!("Inverse: {}", current.title),
                    origin: None,
                    inverse_of: Some(current.change_id.clone()),
                    allocated_ids: BTreeMap::new(),
                    read_preconditions: Vec::new(),
                    operations,
                });
            }
            let Some(parent_id) = current.inverse_of.clone() else {
                let draft = inverse.expect("first replacement validated");
                if draft.title.len() > 4096 {
                    return Err(WikiError::invalid(
                        "Page inverse title exceeds projection limit",
                    ));
                }
                self.plan(&draft)?;
                return Ok(ValidatedPageInverse { draft });
            };
            if depth == 64 {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "Page inverse ancestry exceeds64 hops",
                ));
            }
            let (parent, parent_hash) =
                self.load_manifest_with_budget(&parent_id, &mut remaining)?;
            exact_page_inverse(&current, &parent)?;
            current = parent;
            current_hash = parent_hash;
        }
        Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "Page inverse ancestry exceeds64 hops",
        ))
    }

    fn page_replacement_reversal(
        &self,
        manifest: &ChangeManifest,
        remaining: &mut usize,
    ) -> Result<Vec<ExpectedWrite>> {
        page_replacement_shape(manifest)?;
        let mut operations = Vec::new();
        let mut identities = BTreeSet::new();
        for (index, operation) in manifest.operations.iter().enumerate() {
            let mut payloads = Vec::new();
            for (side, expected, payload) in [
                ("before", &operation.before, &operation.before_payload),
                ("proposed", &operation.after, &operation.after_payload),
            ] {
                let bytes = self
                    .verify_payload_with_limit(
                        &manifest.change_id,
                        index,
                        side,
                        &operation.target,
                        (expected, payload),
                        (*remaining).min(PAGE_INVERSE_BYTES),
                    )?
                    .ok_or_else(|| {
                        WikiError::invalid(
                            "Page inverse requires two retained replacement payloads",
                        )
                    })?;
                *remaining = remaining
                    .checked_sub(bytes.len())
                    .ok_or_else(page_inverse_budget)?;
                payloads.push(bytes);
            }
            let before = parse_note(&payloads[0]);
            let after = parse_note(&payloads[1]);
            let valid = before
                .canonical
                .as_ref()
                .zip(after.canonical.as_ref())
                .filter(|(before, after)| {
                    before.kind() == RecordKind::Page
                        && after.kind() == RecordKind::Page
                        && before.id() == after.id()
                })
                .ok_or_else(|| {
                    WikiError::invalid("Page inverse payloads change identity or kind")
                })?;
            if !identities.insert(valid.0.id().clone()) {
                return Err(WikiError::invalid("Page inverse has duplicate identities"));
            }
            operations.push(ExpectedWrite {
                target: operation.target.clone(),
                expected: operation.after.clone(),
                proposed: Some(payloads.remove(0)),
                apply_after: Vec::new(),
            });
        }
        Ok(operations)
    }

    pub fn inverse_plan(&self, change: &PreparedChange) -> Result<InversePlan> {
        self.require_binding()?;
        let mut remaining = super::prepare::MAX_INVERSE_PAYLOAD_BYTES;
        let (manifest, hash) = self.load_manifest_with_budget(&change.change_id, &mut remaining)?;
        if hash != change.manifest_hash {
            return Err(WikiError::invalid("prepared manifest binding changed"));
        }
        let mut operations = Vec::new();
        let mut retained_paths = Vec::new();
        for (index, op) in manifest.operations.iter().enumerate() {
            if op.role == OperationRole::ImmutableAsset {
                retained_paths.push(op.target.clone());
                continue;
            }
            if op
                .before_payload
                .as_ref()
                .is_some_and(|payload| payload.byte_len > remaining as u64)
            {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "inverse read ceiling",
                ));
            }
            let proposed = self.verify_payload_with_limit(
                &manifest.change_id,
                index,
                "before",
                &op.target,
                (&op.before, &op.before_payload),
                remaining,
            )?;
            remaining = remaining
                .checked_sub(proposed.as_ref().map_or(0, Vec::len))
                .ok_or_else(|| WikiError::new(ErrorCode::BudgetExceeded, "inverse read ceiling"))?;
            operations.push(ExpectedWrite {
                target: op.target.clone(),
                expected: op.after.clone(),
                proposed,
                apply_after: Vec::new(),
            });
        }
        // Reverse original ordering: rename source is restored before its destination is removed.
        for (index, op) in manifest.operations.iter().enumerate() {
            for dependency in &op.apply_after {
                let dependency_target = &manifest.operations[*dependency].target;
                if operations.iter().any(|o| o.target == op.target)
                    && let Some(inverse) = operations
                        .iter_mut()
                        .find(|o| &o.target == dependency_target)
                {
                    inverse
                        .apply_after
                        .push(manifest.operations[index].target.clone());
                }
            }
        }
        let draft = ChangeDraft {
            title: format!("Inverse: {}", manifest.title),
            origin: None,
            inverse_of: Some(manifest.change_id),
            allocated_ids: BTreeMap::new(),
            read_preconditions: Vec::new(),
            operations,
        };
        self.plan(&draft)?; // Preserves unfamiliar edits and immutable source rules in read-only planning.
        // A source-add inverse cannot remove its mutable manifest while its
        // immutable revision tree remains. Explain this specific rejected
        // operation before generic graph validation reports orphaned revisions.
        for operation in &draft.operations {
            let path = operation.target.as_str();
            if operation.proposed.is_some()
                || !path.starts_with("sources/")
                || !path.ends_with("/source.md")
            {
                continue;
            }
            if remaining == 0 {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "inverse source preflight read ceiling",
                ));
            }
            let Some(bytes) = super::prepare::read_bounded(
                &self.fs,
                &operation.target,
                remaining.min(super::prepare::MAX_PAYLOAD_BYTES),
            )?
            else {
                return Err(WikiError::new(
                    ErrorCode::ContentConflict,
                    format!(
                        "source changed after inverse preflight: {}",
                        operation.target
                    ),
                ));
            };
            remaining = remaining.checked_sub(bytes.len()).ok_or_else(|| {
                WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "inverse source preflight read ceiling",
                )
            })?;
            if operation.expected != ExpectedState::Hash(Blake3Hash::digest(&bytes)) {
                return Err(WikiError::new(
                    ErrorCode::ContentConflict,
                    format!(
                        "source changed after inverse preflight: {}",
                        operation.target
                    ),
                ));
            }
            let note = parse_note(&bytes);
            let Some(source) = note
                .canonical
                .as_ref()
                .filter(|record| record.kind() == RecordKind::Source)
            else {
                continue;
            };
            let retained_revision = source
                .field("wiki_revisions")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|revisions| {
                    revisions
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .any(|id| {
                            let revision_path =
                                format!("sources/{}/revisions/{id}/revision.md", source.id());
                            retained_paths
                                .iter()
                                .any(|path| path.as_str() == revision_path)
                        })
                });
            if retained_revision {
                return Err(WikiError::new(
                    ErrorCode::RecordInvalid,
                    format!(
                        "rollback would orphan immutable revisions of source {}; use `source withdraw {} --reason ...` to retain their history",
                        source.id(),
                        source.id()
                    ),
                ));
            }
        }
        Ok(InversePlan {
            draft,
            retained_paths,
        })
    }
    pub fn prepare_inverse(
        &self,
        permit: &WriterPermit,
        change: &PreparedChange,
        validator: &dyn GraphValidator,
    ) -> Result<PreparedChange> {
        permit.require_root(self.fs.root())?;
        let plan = self.inverse_plan(change)?;
        let input = ValidationInput {
            vault_id: self.vault_id.clone(),
            documents: self.scan_documents_bounded(super::prepare::MAX_GRAPH_INPUT_BYTES, 4096)?,
            overlay: plan
                .draft
                .operations
                .iter()
                .map(|o| ProposedTarget {
                    path: o.target.clone(),
                    bytes: o.proposed.clone(),
                })
                .collect(),
        };
        self.validate_draft_graph_with_input(&plan.draft, validator, &input)?;
        Ok(self.prepare(permit, plan.draft)?.prepared)
    }
    /// Read-only semantic preflight; authorities derive from actual retained history.
    pub fn validate_draft_graph(
        &self,
        draft: &ChangeDraft,
        validator: &dyn GraphValidator,
    ) -> Result<ValidatedGraph> {
        self.plan(draft)?;
        let input = ValidationInput {
            vault_id: self.vault_id.clone(),
            documents: if draft.inverse_of.is_some()
                || draft.origin.as_ref().is_some_and(|origin| {
                    matches!(
                        origin.operation,
                        OriginOperation::GraphDecide | OriginOperation::GraphReview
                    )
                }) {
                self.scan_documents_bounded(super::prepare::MAX_GRAPH_INPUT_BYTES, 4096)?
            } else {
                self.scan_documents()?
            },
            overlay: draft
                .operations
                .iter()
                .map(|op| ProposedTarget {
                    path: op.target.clone(),
                    bytes: op.proposed.clone(),
                })
                .collect(),
        };
        self.validate_draft_graph_with_input(draft, validator, &input)
    }
    fn validate_draft_graph_with_input(
        &self,
        draft: &ChangeDraft,
        validator: &dyn GraphValidator,
        input: &ValidationInput,
    ) -> Result<ValidatedGraph> {
        if draft.inverse_of.is_none() {
            return validator.validate(&self.fs, input);
        }
        let preview = self.draft_manifest(draft)?;
        match self.retained_inverse_input(&preview)? {
            Some(inverse) => validator.validate_inverse(&self.fs, input, &inverse),
            None => validator.validate(&self.fs, input),
        }
    }
    /// An internal operation preview carries no allocated identity or retained status.
    fn draft_manifest(&self, draft: &ChangeDraft) -> Result<ChangeManifest> {
        let plan = self.plan(draft)?;
        let targets = plan
            .operations
            .iter()
            .map(|op| op.target.clone())
            .collect::<Vec<_>>();
        let operations = plan
            .operations
            .iter()
            .zip(&plan.roles)
            .map(|(op, role)| {
                Ok(ChangeOp {
                    target: op.target.clone(),
                    before: op.expected.clone(),
                    after: op.proposed.as_ref().map_or(ExpectedState::Absent, |bytes| {
                        ExpectedState::Hash(Blake3Hash::digest(bytes))
                    }),
                    before_payload: None,
                    after_payload: None,
                    role: *role,
                    apply_after: op
                        .apply_after
                        .iter()
                        .map(|path| {
                            targets
                                .iter()
                                .position(|target| target == path)
                                .ok_or_else(|| WikiError::invalid("inverse ordering target absent"))
                        })
                        .collect::<Result<Vec<_>>>()?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(ChangeManifest {
            version: 1,
            vault_id: self.vault_id.clone(),
            // This private preview is never persisted or returned as a change.
            change_id: draft
                .inverse_of
                .clone()
                .unwrap_or_else(|| self.vault_id.clone()),
            title: draft.title.clone(),
            created_at: String::new(),
            origin: draft.origin.clone(),
            inverse_of: draft.inverse_of.clone(),
            allocated_ids: draft.allocated_ids.clone(),
            read_preconditions: plan.read_preconditions,
            operations,
        })
    }
    /// Read-only validation for an already retained proposal, including partial recovery.
    pub fn validate_prepared_graph(
        &self,
        change: &PreparedChange,
        validator: &dyn GraphValidator,
    ) -> Result<ValidatedGraph> {
        self.require_binding()?;
        let (manifest, hash) = self.load_manifest(&change.change_id)?;
        if hash != change.manifest_hash {
            return Err(WikiError::invalid("prepared manifest binding changed"));
        }
        let input = self.validation_input(&manifest)?;
        self.validate_graph(&manifest, &hash, validator, &input)
    }
    pub(super) fn retained_inverse_input(
        &self,
        inverse: &ChangeManifest,
    ) -> Result<Option<RetainedGraphInverseInput>> {
        let Some(mut parent_id) = inverse.inverse_of.clone() else {
            return Ok(None);
        };
        let mut remaining = super::prepare::MAX_INVERSE_PAYLOAD_BYTES;
        let mut seen = BTreeSet::new();
        let mut child = inverse.clone();
        let mut immediate = None;
        for depth in 1..=64 {
            if !seen.insert(parent_id.clone()) {
                return Err(WikiError::invalid("inverse ancestry cycle"));
            }
            let (parent, hash) = self.load_manifest_with_budget(&parent_id, &mut remaining)?;
            let committed = match outcome::terminal_report(&self.fs, &parent, &hash)? {
                Some(report) => report.status == ChangeStatus::Committed,
                None => {
                    journal::load_journal(&self.fs, &parent, &hash)?.status
                        == Some(ChangeStatus::Committed)
                }
            };
            if !committed {
                return Err(WikiError::new(
                    ErrorCode::RecoveryRequired,
                    "inverse requires authenticated committed original history",
                ));
            }
            exact_inverse(&child, &parent)?;
            if immediate.is_none() {
                immediate = Some((parent.clone(), hash.clone()));
            }
            if parent.origin.as_ref().is_some_and(|origin| {
                matches!(
                    origin.operation,
                    OriginOperation::GraphDecide | OriginOperation::GraphReview
                )
            }) {
                if parent.inverse_of.is_some() {
                    return Err(WikiError::invalid(
                        "graph decision anchor cannot also claim inverse ancestry",
                    ));
                }
                let (first, first_hash) = immediate.expect("first parent retained");
                let parent_operations = if first.change_id == parent.change_id {
                    None
                } else {
                    Some(self.retained_graph_operations(&first, &mut remaining)?)
                };
                let anchor = self.retained_graph_input(&parent, &hash, &mut remaining)?;
                return Ok(Some(RetainedGraphInverseInput {
                    anchor,
                    parent_change_id: first.change_id,
                    parent_manifest_hash: first_hash,
                    parent_operations,
                    inversion_depth: depth,
                }));
            }
            let Some(next) = parent.inverse_of.clone() else {
                return Ok(None);
            };
            child = parent;
            parent_id = next;
        }
        Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "inverse ancestry exceeds64 hops",
        ))
    }
}

fn exact_inverse(child: &ChangeManifest, parent: &ChangeManifest) -> Result<()> {
    if child.inverse_of.as_ref() != Some(&parent.change_id)
        || child.origin.is_some()
        || !child.allocated_ids.is_empty()
        || !child.read_preconditions.is_empty()
    {
        return Err(WikiError::invalid(
            "inverse contains non-reversal authority",
        ));
    }
    let originals = parent
        .operations
        .iter()
        .filter(|op| op.role != OperationRole::ImmutableAsset)
        .collect::<Vec<_>>();
    if originals.len() != child.operations.len() {
        return Err(WikiError::invalid(
            "inverse mutable operation membership differs",
        ));
    }
    let mut order: BTreeMap<_, BTreeSet<_>> = originals
        .iter()
        .map(|op| (op.target.clone(), BTreeSet::new()))
        .collect();
    for op in &originals {
        for index in &op.apply_after {
            let dependency = &parent.operations[*index];
            if let Some(after) = order.get_mut(&dependency.target) {
                after.insert(op.target.clone());
            }
        }
    }
    for (op, original) in child.operations.iter().zip(originals) {
        let actual_order = op
            .apply_after
            .iter()
            .map(|index| {
                child
                    .operations
                    .get(*index)
                    .map(|op| op.target.clone())
                    .ok_or_else(|| WikiError::invalid("inverse ordering index invalid"))
            })
            .collect::<Result<BTreeSet<_>>>()?;
        if op.target != original.target
            || op.before != original.after
            || op.after != original.before
            || op.role != OperationRole::MutableRecord
            || actual_order != order[&op.target]
        {
            return Err(WikiError::invalid(
                "inverse is not the exact guarded mutable reversal",
            ));
        }
    }
    Ok(())
}

fn page_inverse_budget() -> WikiError {
    WikiError::new(
        ErrorCode::BudgetExceeded,
        "Page inverse aggregate read ceiling",
    )
}

fn page_replacement_shape(manifest: &ChangeManifest) -> Result<()> {
    if manifest.origin.is_some()
        || !manifest.allocated_ids.is_empty()
        || manifest.operations.is_empty()
        || manifest.operations.len() > 16
        || manifest.read_preconditions.len() > 128
        || manifest.title.trim().is_empty()
        || manifest.title.len() > 4096
    {
        return Err(WikiError::invalid(
            "Page inverse requires a bounded ordinary replacement history",
        ));
    }
    let mut paths = BTreeSet::new();
    let mut before_bytes = 0usize;
    let mut after_bytes = 0usize;
    for op in &manifest.operations {
        if op.role != OperationRole::MutableRecord
            || !op.apply_after.is_empty()
            || !crate::sources::revision::canonical_path(&op.target)
            || !paths.insert(op.target.clone())
            || !matches!(op.before, ExpectedState::Hash(_))
            || !matches!(op.after, ExpectedState::Hash(_))
        {
            return Err(WikiError::invalid(
                "Page inverse excludes assets, creation, deletion, rename and ordered operations",
            ));
        }
        for (payload, total) in [
            (&op.before_payload, &mut before_bytes),
            (&op.after_payload, &mut after_bytes),
        ] {
            let size = payload
                .as_ref()
                .ok_or_else(|| WikiError::invalid("Page inverse replacement payload missing"))?
                .byte_len;
            *total = total
                .checked_add(usize::try_from(size).map_err(|_| page_inverse_budget())?)
                .ok_or_else(page_inverse_budget)?;
            if *total > PAGE_INVERSE_BYTES {
                return Err(page_inverse_budget());
            }
        }
    }
    Ok(())
}

/// Page projection may add authenticated read-only constraints. They grant no
/// write authority; the global graph inverse validator remains strict.
fn exact_page_inverse(child: &ChangeManifest, parent: &ChangeManifest) -> Result<()> {
    page_replacement_shape(child)?;
    page_replacement_shape(parent)?;
    let mut writes = child.clone();
    writes.read_preconditions.clear();
    exact_inverse(&writes, parent)
}

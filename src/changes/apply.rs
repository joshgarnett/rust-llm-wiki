//! Forward application, whole-vault verification, and guarded publication.
use super::{
    journal, outcome,
    prepare::{MAX_PAYLOAD_BYTES, read_bounded, topological_order},
    types::*,
};
use crate::{
    domain::{Blake3Hash, ErrorCode, Result, VaultRelativePath, WikiError},
    vault::{ExpectedState, WriterPermit},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

impl ChangeEngine {
    pub fn apply(
        &self,
        permit: &WriterPermit,
        change: &PreparedChange,
        validator: &dyn GraphValidator,
        publisher: &dyn PublicationBackend,
    ) -> Result<ApplyReport> {
        permit.require_root(self.fs.root())?;
        self.require_binding()?;
        let (manifest, hash) = self.load_manifest(&change.change_id)?;
        if hash != change.manifest_hash {
            return Err(WikiError::invalid("prepared manifest binding changed"));
        }
        if let Some(report) = outcome::terminal_report(&self.fs, &manifest, &hash)? {
            outcome::sync_receipt(&self.fs, permit, &manifest.change_id)?;
            return Ok(report);
        }
        let state = journal::load_journal(&self.fs, &manifest, &hash)?;
        if matches!(
            state.status,
            Some(ChangeStatus::Committed | ChangeStatus::Aborted)
        ) {
            return outcome::retain_terminal(self, permit, &manifest, &hash, &state);
        }
        if state.status == Some(ChangeStatus::Conflict) {
            return Err(recovery_error(
                "change has a durable conflict requiring explicit resolution",
            ));
        }
        self.continue_apply(permit, change, &manifest, &state, validator, publisher)
    }

    pub(crate) fn continue_apply(
        &self,
        permit: &WriterPermit,
        change: &PreparedChange,
        manifest: &ChangeManifest,
        initial: &JournalState,
        validator: &dyn GraphValidator,
        publisher: &dyn PublicationBackend,
    ) -> Result<ApplyReport> {
        permit.require_root(self.fs.root())?;
        publisher.check_available()?;
        self.verify_read_preconditions(permit, manifest, &change.manifest_hash)?;
        self.require_revision_baseline(manifest)?;
        let observations = self.observe(manifest)?;
        if initial.status == Some(ChangeStatus::Prepared)
            && observations.iter().any(|o| o.observed != o.before)
        {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "staged proposal no longer matches its prepared expectations",
            ));
        }
        if observations
            .iter()
            .any(|o| o.observed != o.before && o.observed != o.after)
        {
            return self.conflict(
                permit,
                manifest,
                &change.manifest_hash,
                "preflight",
                observations,
            );
        }
        self.require_no_other_unresolved(change)?;
        let input = self.validation_input(manifest)?;
        let graph = self.validate_graph(manifest, &change.manifest_hash, validator, &input)?;
        let expected_scan = projected_scan(&input);
        self.retain_validation(permit, change, manifest, initial, &graph, &expected_scan)?;
        self.verify_read_preconditions(permit, manifest, &change.manifest_hash)?;
        if let Err(error) =
            self.preflight_revision_trees(permit, manifest, &change.manifest_hash, initial)
        {
            return self.revision_failure(permit, manifest, &change.manifest_hash, initial, error);
        }
        self.apply_files_to_files_applied(
            permit,
            change,
            manifest,
            initial,
            &observations,
            &|complete| {
                self.guard_revision_trees(permit, manifest, &change.manifest_hash, complete)
            },
        )?;
        self.require_all_after(permit, manifest, &change.manifest_hash)?;
        self.require_no_other_unresolved(change)?;
        let final_input = self.validation_input(manifest)?;
        if document_map(&final_input) != expected_scan {
            return self.conflict(
                permit,
                manifest,
                &change.manifest_hash,
                "canonical scan changed during application",
                self.observe(manifest)?,
            );
        }
        let final_graph =
            self.validate_graph(manifest, &change.manifest_hash, validator, &final_input)?;
        if graph != final_graph {
            return self.conflict(
                permit,
                manifest,
                &change.manifest_hash,
                "graph fingerprints or read dependencies changed during application",
                self.observe(manifest)?,
            );
        }
        self.verify_dependencies(permit, manifest, &change.manifest_hash, &graph)?;
        // Recheck the exact scan after validation, including added and deleted unrelated records.
        if document_map(&self.validation_input(manifest)?) != document_map(&final_input) {
            return self.conflict(
                permit,
                manifest,
                &change.manifest_hash,
                "canonical scan changed during validation",
                self.observe(manifest)?,
            );
        }
        self.verify_dependencies(permit, manifest, &change.manifest_hash, &graph)?;
        self.guard_revision_trees(permit, manifest, &change.manifest_hash, true)?;
        self.verify_read_preconditions(permit, manifest, &change.manifest_hash)?;
        let authority = PublicationPermit {
            writer: permit,
            vault_id: &self.vault_id,
            change,
            graph: &graph,
        };
        let snapshot = publisher.publish(&self.fs, &authority, &final_input)?;
        if snapshot.parser_fingerprint != graph.parser_fingerprint
            || snapshot.require_canonical_manifest()? != &graph.control_manifest
        {
            return Err(recovery_error(
                "publisher returned a snapshot outside the verified graph",
            ));
        }
        self.guard_revision_trees(permit, manifest, &change.manifest_hash, true)?;
        self.require_all_after(permit, manifest, &change.manifest_hash)?;
        if document_map(&self.validation_input(manifest)?) != document_map(&final_input) {
            return self.conflict(
                permit,
                manifest,
                &change.manifest_hash,
                "canonical scan changed during publication",
                self.observe(manifest)?,
            );
        }
        self.verify_dependencies(permit, manifest, &change.manifest_hash, &graph)?;
        self.verify_read_preconditions(permit, manifest, &change.manifest_hash)?;
        journal::append_event(
            &self.fs,
            permit,
            manifest,
            &change.manifest_hash,
            ChangeEvent::Indexed { snapshot },
        )?;
        self.verify_read_preconditions(permit, manifest, &change.manifest_hash)?;
        outcome::finish(
            self,
            permit,
            manifest,
            &change.manifest_hash,
            ChangeStatus::Committed,
        )
    }

    /// Common mutation/journal path. Callers establish and retain their typed
    /// validation baseline before entering, and supply the matching immutable
    /// tree guard. Publication and whole-graph validation stay with the caller.
    pub(super) fn apply_files_to_files_applied(
        &self,
        permit: &WriterPermit,
        change: &PreparedChange,
        manifest: &ChangeManifest,
        initial: &JournalState,
        observations: &[TargetObservation],
        guard_revision_trees: &dyn Fn(bool) -> Result<()>,
    ) -> Result<JournalState> {
        let mut state = initial.clone();
        if state.status.is_none() {
            state = journal::append_event(
                &self.fs,
                permit,
                manifest,
                &change.manifest_hash,
                ChangeEvent::Prepared,
            )?;
        }
        if state.status == Some(ChangeStatus::Prepared) {
            state = journal::append_event(
                &self.fs,
                permit,
                manifest,
                &change.manifest_hash,
                ChangeEvent::Applying,
            )?;
        }
        let replay = matches!(
            state.status,
            Some(ChangeStatus::FilesApplied | ChangeStatus::Indexed)
        ) && observations.iter().any(|o| o.observed == o.before)
            || state.status == Some(ChangeStatus::Applying)
                && state
                    .frames
                    .iter()
                    .filter_map(|f| match f.event {
                        ChangeEvent::Done { op } => Some(op),
                        _ => None,
                    })
                    .any(|op| observations[op].observed == observations[op].before);
        if replay {
            state = journal::append_event(
                &self.fs,
                permit,
                manifest,
                &change.manifest_hash,
                ChangeEvent::Applying,
            )?;
        }
        if state.status == Some(ChangeStatus::Applying) {
            let epoch = state
                .frames
                .iter()
                .rposition(|f| {
                    matches!(
                        f.event,
                        ChangeEvent::Applying
                            | ChangeEvent::ResolutionAccepted {
                                mode: ConflictResolutionMode::Resume,
                                ..
                            }
                    )
                })
                .expect("applying epoch");
            let completed: BTreeSet<_> = state.frames[epoch..]
                .iter()
                .filter_map(|f| match f.event {
                    ChangeEvent::Done { op } => Some(op),
                    _ => None,
                })
                .collect();
            let mut intent = None;
            for frame in &state.frames[epoch..] {
                match frame.event {
                    ChangeEvent::Intent { op } => intent = Some(op),
                    ChangeEvent::Done { .. } => intent = None,
                    _ => {}
                }
            }
            let dependencies: Vec<_> = manifest
                .operations
                .iter()
                .map(|o| o.apply_after.clone())
                .collect();
            for index in topological_order(&dependencies)? {
                self.verify_read_preconditions(permit, manifest, &change.manifest_hash)?;
                guard_revision_trees(false)?;
                let operation = &manifest.operations[index];
                let observed = self.target_state(&operation.target)?;
                if observed != operation.before && observed != operation.after {
                    return self.conflict(
                        permit,
                        manifest,
                        &change.manifest_hash,
                        "operation recheck",
                        self.observe(manifest)?,
                    );
                }
                let already_done = completed.contains(&index);
                // A completed target may have reverted to its old state after the interruption.
                // Its existing durable intent authorizes redoing it; its flag is never evidence of bytes.
                let staged = if observed == operation.before {
                    if let Some(bytes) = self.verify_payload(
                        &manifest.change_id,
                        index,
                        "proposed",
                        &operation.target,
                        &operation.after,
                        &operation.after_payload,
                    )? {
                        if let Some((parent, _)) = operation.target.as_str().rsplit_once('/') {
                            journal::require_sync(
                                self.fs
                                    .ensure_directory(&VaultRelativePath::new(parent)?, permit)?,
                            )?;
                        }
                        Some(self.fs.stage(&operation.target, &bytes, permit)?)
                    } else {
                        None
                    }
                } else {
                    None
                };
                if !already_done && intent != Some(index) {
                    if intent.is_some() {
                        return Err(WikiError::invalid(
                            "journal intent conflicts with application order",
                        ));
                    }
                    journal::append_event(
                        &self.fs,
                        permit,
                        manifest,
                        &change.manifest_hash,
                        ChangeEvent::Intent { op: index },
                    )?;
                }
                guard_revision_trees(false)?;
                self.verify_read_preconditions(permit, manifest, &change.manifest_hash)?;
                let result = if observed == operation.before {
                    match staged {
                        Some(staged) => self.fs.replace(staged, &operation.before, permit),
                        None => self.fs.delete(&operation.target, &operation.before, permit),
                    }
                } else {
                    self.fs.sync_target(&operation.target, permit)
                };
                match result {
                    Ok(sync) => journal::require_sync(sync)?,
                    Err(error) if error.code == ErrorCode::ContentConflict => {
                        return self.conflict(
                            permit,
                            manifest,
                            &change.manifest_hash,
                            "guarded mutation",
                            self.observe(manifest)?,
                        );
                    }
                    Err(error) => return Err(error),
                }
                guard_revision_trees(false)?;
                self.verify_read_preconditions(permit, manifest, &change.manifest_hash)?;
                if self.target_state(&operation.target)? != operation.after {
                    return self.conflict(
                        permit,
                        manifest,
                        &change.manifest_hash,
                        "post-mutation",
                        self.observe(manifest)?,
                    );
                }
                if !already_done {
                    journal::append_event(
                        &self.fs,
                        permit,
                        manifest,
                        &change.manifest_hash,
                        ChangeEvent::Done { op: index },
                    )?;
                    intent = None;
                }
            }
            guard_revision_trees(true)?;
            self.verify_read_preconditions(permit, manifest, &change.manifest_hash)?;
            self.require_all_after(permit, manifest, &change.manifest_hash)?;
            state = journal::append_event(
                &self.fs,
                permit,
                manifest,
                &change.manifest_hash,
                ChangeEvent::FilesApplied,
            )?;
        }
        if !matches!(
            state.status,
            Some(ChangeStatus::FilesApplied | ChangeStatus::Indexed)
        ) {
            return Err(recovery_error("change cannot publish in its current state"));
        }
        Ok(state)
    }

    fn guard_revision_trees(
        &self,
        permit: &WriterPermit,
        manifest: &ChangeManifest,
        hash: &Blake3Hash,
        complete: bool,
    ) -> Result<()> {
        if let Err(error) = self.verify_revision_trees(permit, manifest, hash, complete) {
            let state = journal::load_journal(&self.fs, manifest, hash)?;
            return self.revision_failure(permit, manifest, hash, &state, error);
        }
        Ok(())
    }
    pub(super) fn revision_failure<T>(
        &self,
        permit: &WriterPermit,
        manifest: &ChangeManifest,
        hash: &Blake3Hash,
        state: &JournalState,
        error: WikiError,
    ) -> Result<T> {
        if matches!(
            state.status,
            Some(ChangeStatus::Applying | ChangeStatus::FilesApplied | ChangeStatus::Indexed)
        ) && matches!(
            error.code,
            ErrorCode::ContentConflict | ErrorCode::RecordInvalid
        ) {
            let phase: String = format!("immutable revision tree: {}", error.message)
                .chars()
                .take(240)
                .collect();
            // Extra members are not manifest operations. Record the exact path in the phase;
            // do not invent an operation observation or read through an unfamiliar symlink.
            return self.conflict(permit, manifest, hash, &phase, Vec::new());
        }
        Err(error)
    }
    pub(crate) fn retain_validation(
        &self,
        permit: &WriterPermit,
        change: &PreparedChange,
        manifest: &ChangeManifest,
        state: &JournalState,
        graph: &ValidatedGraph,
        scan: &BTreeMap<VaultRelativePath, Blake3Hash>,
    ) -> Result<()> {
        let path = VaultRelativePath::new(format!("changes/{}/validation.json", change.change_id))?;
        let proof = ValidationProof {
            version: 1,
            change: change.clone(),
            parser_fingerprint: graph.parser_fingerprint.clone(),
            control_manifest: graph.control_manifest.clone(),
            dependencies: graph.dependencies.clone(),
            scan: scan.clone(),
        };
        if let Some(bytes) = read_bounded(&self.fs, &path, super::prepare::MAX_JOURNAL_BYTES)? {
            let retained: ValidationReceipt = super::prepare::strict_json(&bytes)?;
            let checksum = Blake3Hash::digest(
                serde_json::to_vec(&retained.proof)
                    .map_err(|e| WikiError::invalid(e.to_string()))?,
            );
            if checksum != retained.checksum
                || retained.proof.version != 1
                || retained.proof.change != *change
            {
                return Err(WikiError::invalid(
                    "retained prevalidation integrity failure",
                ));
            }
            if retained.proof != proof {
                if matches!(
                    state.status,
                    None | Some(ChangeStatus::Prepared | ChangeStatus::Conflict)
                ) {
                    return Err(WikiError::new(
                        ErrorCode::ContentConflict,
                        "retained whole-graph prevalidation is stale; restore its original scope before resuming",
                    ));
                }
                return self.conflict(
                    permit,
                    manifest,
                    &change.manifest_hash,
                    "retained whole-graph validation changed",
                    self.observe(manifest)?,
                );
            }
            journal::require_sync(self.fs.sync_target(&path, permit)?)?;
            return Ok(());
        }
        if matches!(
            state.status,
            Some(ChangeStatus::Applying | ChangeStatus::FilesApplied | ChangeStatus::Indexed)
        ) {
            return Err(recovery_error(
                "applying journal lost its retained whole-graph validation",
            ));
        }
        let checksum = Blake3Hash::digest(
            serde_json::to_vec(&proof).map_err(|e| WikiError::invalid(e.to_string()))?,
        );
        let bytes = serde_json::to_vec(&ValidationReceipt { proof, checksum })
            .map_err(|e| WikiError::invalid(e.to_string()))?;
        if bytes.len() > super::prepare::MAX_JOURNAL_BYTES {
            return Err(WikiError::invalid("validation receipt exceeds limit"));
        }
        let staged = self.fs.stage(&path, &bytes, permit)?;
        journal::require_sync(self.fs.replace(staged, &ExpectedState::Absent, permit)?)
    }
    pub(crate) fn target_state(&self, target: &VaultRelativePath) -> Result<ExpectedState> {
        Ok(
            read_bounded(&self.fs, target, MAX_PAYLOAD_BYTES)?.map_or(ExpectedState::Absent, |b| {
                ExpectedState::Hash(Blake3Hash::digest(b))
            }),
        )
    }
    pub(crate) fn observe(&self, manifest: &ChangeManifest) -> Result<Vec<TargetObservation>> {
        manifest
            .operations
            .iter()
            .map(|op| {
                Ok(TargetObservation {
                    target: op.target.clone(),
                    before: op.before.clone(),
                    after: op.after.clone(),
                    observed: self.target_state(&op.target)?,
                })
            })
            .collect()
    }
    pub(crate) fn conflict<T>(
        &self,
        permit: &WriterPermit,
        manifest: &ChangeManifest,
        hash: &Blake3Hash,
        phase: &str,
        observations: Vec<TargetObservation>,
    ) -> Result<T> {
        let state = journal::load_journal(&self.fs, manifest, hash)?;
        if state.status.is_none() {
            journal::append_event(&self.fs, permit, manifest, hash, ChangeEvent::Prepared)?;
        }
        journal::append_event(
            &self.fs,
            permit,
            manifest,
            hash,
            ChangeEvent::Conflict {
                phase: phase.into(),
                observations,
            },
        )?;
        Err(WikiError::new(
            ErrorCode::ContentConflict,
            format!("change conflict during {phase}; unfamiliar bytes preserved"),
        ))
    }
    pub(super) fn require_all_after(
        &self,
        permit: &WriterPermit,
        manifest: &ChangeManifest,
        hash: &Blake3Hash,
    ) -> Result<()> {
        let observations = self.observe(manifest)?;
        if observations.iter().any(|o| o.observed != o.after) {
            return self.conflict(
                permit,
                manifest,
                hash,
                "files-applied verification",
                observations,
            );
        }
        Ok(())
    }
    pub(super) fn validate_graph(
        &self,
        manifest: &ChangeManifest,
        manifest_hash: &Blake3Hash,
        validator: &dyn GraphValidator,
        input: &ValidationInput,
    ) -> Result<ValidatedGraph> {
        if manifest.inverse_of.is_some() {
            return match self.retained_inverse_input(manifest)? {
                Some(inverse) => validator.validate_inverse(&self.fs, input, &inverse),
                None => validator.validate(&self.fs, input),
            };
        }
        let Some(origin) = &manifest.origin else {
            return validator.validate(&self.fs, input);
        };
        if !matches!(
            origin.operation,
            OriginOperation::GraphDecide | OriginOperation::GraphReview
        ) {
            return validator.validate(&self.fs, input);
        }
        let mut remaining = 128usize * 1024 * 1024;
        let retained = self.retained_graph_input(manifest, manifest_hash, &mut remaining)?;
        validator.validate_retained(&self.fs, input, &retained)
    }
    pub(super) fn retained_graph_input(
        &self,
        manifest: &ChangeManifest,
        manifest_hash: &Blake3Hash,
        remaining: &mut usize,
    ) -> Result<RetainedGraphInput> {
        Ok(RetainedGraphInput {
            change_id: manifest.change_id.clone(),
            manifest_hash: manifest_hash.clone(),
            origin: manifest
                .origin
                .clone()
                .ok_or_else(|| WikiError::invalid("retained graph origin missing"))?,
            allocated_ids: manifest.allocated_ids.clone(),
            operations: self.retained_graph_operations(manifest, remaining)?,
        })
    }
    pub(super) fn retained_graph_operations(
        &self,
        manifest: &ChangeManifest,
        remaining: &mut usize,
    ) -> Result<Vec<RetainedGraphOperation>> {
        let mut operations = Vec::with_capacity(manifest.operations.len());
        for (index, operation) in manifest.operations.iter().enumerate() {
            let mut payload = |label,
                               expected: &ExpectedState,
                               reference: &Option<PayloadRef>|
             -> Result<Option<Vec<u8>>> {
                if reference
                    .as_ref()
                    .is_some_and(|r| r.byte_len > *remaining as u64)
                {
                    return Err(WikiError::new(
                        ErrorCode::BudgetExceeded,
                        "retained graph witness exceeds byte ceiling",
                    ));
                }
                let bytes = self.verify_payload_with_limit(
                    &manifest.change_id,
                    index,
                    label,
                    &operation.target,
                    (expected, reference),
                    *remaining,
                )?;
                *remaining = remaining
                    .checked_sub(bytes.as_ref().map_or(0, Vec::len))
                    .ok_or_else(|| {
                        WikiError::new(
                            ErrorCode::BudgetExceeded,
                            "retained graph witness exceeds byte ceiling",
                        )
                    })?;
                Ok(bytes)
            };
            let before_bytes = payload("before", &operation.before, &operation.before_payload)?;
            let after_bytes = payload("proposed", &operation.after, &operation.after_payload)?;
            operations.push(RetainedGraphOperation {
                path: operation.target.clone(),
                role: operation.role,
                before: operation.before.clone(),
                after: operation.after.clone(),
                before_bytes,
                after_bytes,
            });
        }
        Ok(operations)
    }

    pub(super) fn scan_documents(&self) -> Result<Vec<ScanDocument>> {
        self.scan_documents_bounded(usize::MAX, usize::MAX)
    }
    pub(super) fn scan_documents_bounded(
        &self,
        mut remaining: usize,
        max_files: usize,
    ) -> Result<Vec<ScanDocument>> {
        self.require_binding()?;
        let paths = if max_files == usize::MAX {
            self.fs.root().scan_markdown()?
        } else {
            let start = std::time::Instant::now();
            let mut entries = 0usize;
            self.fs.root().scan_markdown_limited(max_files, &mut || {
                if entries >= 65536 || start.elapsed() >= std::time::Duration::from_secs(30) {
                    return Err(WikiError::new(
                        ErrorCode::BudgetExceeded,
                        "graph scan enumeration ceiling",
                    ));
                }
                entries += 1;
                Ok(())
            })?
        };
        if paths.len() > max_files {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "graph scan file ceiling",
            ));
        }
        paths
            .into_iter()
            .map(|path| {
                let bytes = super::prepare::read_with_budget(&self.fs, &path, &mut remaining)?
                    .ok_or_else(|| recovery_error("scan file disappeared"))?;
                Ok(ScanDocument {
                    hash: Blake3Hash::digest(&bytes),
                    path,
                    bytes,
                })
            })
            .collect::<Result<Vec<_>>>()
    }
    pub(crate) fn validation_input(&self, manifest: &ChangeManifest) -> Result<ValidationInput> {
        let bounded_graph = manifest.inverse_of.is_some()
            || manifest.origin.as_ref().is_some_and(|origin| {
                matches!(
                    origin.operation,
                    OriginOperation::GraphDecide | OriginOperation::GraphReview
                )
            });
        let mut remaining = if bounded_graph {
            super::prepare::MAX_GRAPH_INPUT_BYTES
        } else {
            usize::MAX
        };
        let overlay_bytes = manifest
            .operations
            .iter()
            .filter_map(|op| op.after_payload.as_ref())
            .try_fold(0usize, |n, p| {
                usize::try_from(p.byte_len)
                    .ok()
                    .and_then(|len| n.checked_add(len))
            });
        if overlay_bytes.is_none_or(|bytes| bytes > remaining) {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "graph overlay aggregate byte ceiling",
            ));
        }
        let documents = if bounded_graph {
            self.scan_documents_bounded(super::prepare::MAX_GRAPH_INPUT_BYTES, 4096)?
        } else {
            self.scan_documents()?
        };
        let overlay = manifest
            .operations
            .iter()
            .enumerate()
            .map(|(i, op)| {
                let bytes = self.verify_payload_with_limit(
                    &manifest.change_id,
                    i,
                    "proposed",
                    &op.target,
                    (&op.after, &op.after_payload),
                    remaining,
                )?;
                remaining = remaining
                    .checked_sub(bytes.as_ref().map_or(0, Vec::len))
                    .ok_or_else(|| {
                        WikiError::new(ErrorCode::BudgetExceeded, "graph overlay byte ceiling")
                    })?;
                Ok(ProposedTarget {
                    path: op.target.clone(),
                    bytes,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(ValidationInput {
            vault_id: self.vault_id.clone(),
            documents,
            overlay,
        })
    }
    pub(crate) fn verify_read_preconditions(
        &self,
        permit: &WriterPermit,
        manifest: &ChangeManifest,
        hash: &Blake3Hash,
    ) -> Result<()> {
        for condition in &manifest.read_preconditions {
            if self
                .target_state(&condition.path)
                .is_ok_and(|actual| actual == condition.expected)
            {
                continue;
            }
            let phase = format!("read precondition: {}", condition.path);
            let state = journal::load_journal(&self.fs, manifest, hash)?;
            if matches!(
                state.status,
                None | Some(ChangeStatus::Prepared | ChangeStatus::Conflict)
            ) {
                return Err(WikiError::new(ErrorCode::ContentConflict, phase));
            }
            return self.conflict(permit, manifest, hash, &phase, Vec::new());
        }
        Ok(())
    }
    fn verify_dependencies(
        &self,
        permit: &WriterPermit,
        manifest: &ChangeManifest,
        hash: &Blake3Hash,
        graph: &ValidatedGraph,
    ) -> Result<()> {
        let mut seen = BTreeSet::new();
        for dependency in &graph.dependencies {
            if !seen.insert(&dependency.path) {
                return Err(WikiError::invalid(
                    "validated read dependency is duplicated",
                ));
            }
            if self.target_state(&dependency.path)? != dependency.expected {
                return self.conflict(
                    permit,
                    manifest,
                    hash,
                    "validated read dependency changed",
                    self.observe(manifest)?,
                );
            }
        }
        Ok(())
    }
    pub(crate) fn require_no_other_unresolved(&self, current: &PreparedChange) -> Result<()> {
        for id in self.change_ids()? {
            if id == current.change_id {
                continue;
            }
            let (manifest, hash) = self.load_manifest_structure(&id)?;
            if outcome::terminal_report(&self.fs, &manifest, &hash)?.is_some() {
                continue;
            }
            self.validate_manifest(&manifest, &id)?;
            let state = journal::load_journal(&self.fs, &manifest, &hash)?;
            match state.status {
                Some(ChangeStatus::Committed | ChangeStatus::Aborted) => {}
                Some(
                    ChangeStatus::Applying
                    | ChangeStatus::FilesApplied
                    | ChangeStatus::Indexed
                    | ChangeStatus::Conflict,
                ) => {
                    return Err(recovery_error(
                        "another unresolved change blocks publication",
                    ));
                }
                Some(ChangeStatus::Prepared) => {}
                None => {
                    if self
                        .observe(&manifest)?
                        .iter()
                        .any(|o| o.observed != o.before)
                    {
                        return Err(recovery_error(
                            "another staged change has ambiguous target state",
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

fn document_map(input: &ValidationInput) -> BTreeMap<VaultRelativePath, Blake3Hash> {
    input
        .documents
        .iter()
        .map(|d| (d.path.clone(), d.hash.clone()))
        .collect()
}
pub(crate) fn projected_scan(input: &ValidationInput) -> BTreeMap<VaultRelativePath, Blake3Hash> {
    let mut expected = document_map(input);
    for target in &input.overlay {
        if canonical_target(&target.path) {
            match &target.bytes {
                Some(bytes) => {
                    expected.insert(target.path.clone(), Blake3Hash::digest(bytes));
                }
                None => {
                    expected.remove(&target.path);
                }
            }
        }
    }
    expected
}
fn canonical_target(path: &VaultRelativePath) -> bool {
    let s = path.as_str();
    let parts: Vec<_> = s.split('/').collect();
    s.ends_with(".md")
        && parts.last() != Some(&"index.md")
        && !(parts.len() >= 5
            && unicase::UniCase::unicode(parts[0]).to_folded_case() == "sources"
            && unicase::UniCase::unicode(parts[2]).to_folded_case() == "revisions"
            && !(parts.len() == 5 && parts[4] == "revision.md"))
}
pub(crate) fn recovery_error(message: &str) -> WikiError {
    WikiError::new(ErrorCode::RecoveryRequired, message)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidationProof {
    version: u32,
    change: PreparedChange,
    parser_fingerprint: Blake3Hash,
    control_manifest: Blake3Hash,
    dependencies: Vec<ReadDependency>,
    scan: BTreeMap<VaultRelativePath, Blake3Hash>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidationReceipt {
    proof: ValidationProof,
    checksum: Blake3Hash,
}

/// Dispatch may recognize a legacy receipt only after its complete typed
/// envelope and binding have been verified. This grants no replay authority.
pub(super) fn verify_legacy_validation_receipt(
    bytes: &[u8],
    change: &PreparedChange,
) -> Result<()> {
    let receipt: ValidationReceipt = super::prepare::strict_json(bytes)?;
    let checksum = Blake3Hash::digest(
        serde_json::to_vec(&receipt.proof)
            .map_err(|error| WikiError::invalid(error.to_string()))?,
    );
    if receipt.proof.version != 1 || receipt.proof.change != *change || checksum != receipt.checksum
    {
        return Err(recovery_error(
            "invalid legacy validation receipt binding/checksum",
        ));
    }
    Ok(())
}

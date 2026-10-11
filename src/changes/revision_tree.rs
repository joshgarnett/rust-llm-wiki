//! Version-three complete Revision commitments and execution units.
use super::{
    prepare::{MAX_GRAPH_INPUT_BYTES, MAX_PAYLOAD_BYTES, read_bounded},
    types::*,
};
use crate::{
    domain::{Blake3Hash, ErrorCode, RecordKind, Result, VaultRelativePath, WikiError},
    vault::{ExpectedState, VaultFs},
};
use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
#[path = "revision_tree_tests.rs"]
mod tests;

pub(crate) fn require_execution(fs: &VaultFs, manifest: &ChangeManifest) -> Result<()> {
    if manifest.version == 3
        && (!cfg!(feature = "experimental-revision-tree-install")
            || !fs.durable_io().supports_directory_install())
    {
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "complete Revision installation is unavailable in this binary or filesystem",
        ));
    }
    #[cfg(feature = "experimental-revision-tree-install")]
    if manifest.version == 3 {
        validate_structure(manifest)?;
        for group in &manifest.revision_trees {
            fs.admit_revision_tree_input(
                &group.root,
                group.members.iter().map(|&index| {
                    let op = &manifest.operations[index];
                    (
                        op.target.owned_capacity(),
                        op.after_payload.as_ref().unwrap().byte_len as usize,
                    )
                }),
            )?;
        }
    }
    Ok(())
}

pub(crate) fn grouped(manifest: &ChangeManifest, op: usize) -> Option<usize> {
    manifest
        .revision_trees
        .iter()
        .position(|g| g.members.binary_search(&op).is_ok())
}

/// Units are represented by their smallest operation index, preserving stable order.
pub(crate) fn execution_order(manifest: &ChangeManifest) -> Result<Vec<usize>> {
    let mut representatives: Vec<_> = (0..manifest.operations.len()).collect();
    for group in &manifest.revision_trees {
        let first = *group
            .members
            .first()
            .ok_or_else(|| WikiError::invalid("empty Revision group"))?;
        for &member in &group.members {
            let slot = representatives
                .get_mut(member)
                .ok_or_else(|| WikiError::invalid("Revision member index out of range"))?;
            *slot = first;
        }
    }
    let mut edges = BTreeMap::<usize, BTreeSet<usize>>::new();
    for (index, op) in manifest.operations.iter().enumerate() {
        let unit = representatives[index];
        let prerequisites = edges.entry(unit).or_default();
        for &dependency in &op.apply_after {
            let other = *representatives
                .get(dependency)
                .ok_or_else(|| WikiError::invalid("dependency index out of range"))?;
            if other != unit {
                prerequisites.insert(other);
            }
        }
    }
    let mut order = Vec::new();
    while !edges.is_empty() {
        let next = edges
            .iter()
            .find_map(|(&unit, prerequisites)| prerequisites.is_empty().then_some(unit))
            .ok_or_else(|| WikiError::invalid("Revision contraction creates a dependency cycle"))?;
        edges.remove(&next);
        for prerequisites in edges.values_mut() {
            prerequisites.remove(&next);
        }
        order.push(next);
    }
    Ok(order)
}

pub(crate) fn validate_structure(manifest: &ChangeManifest) -> Result<()> {
    if manifest.version != 3 {
        return if manifest.revision_trees.is_empty() {
            Ok(())
        } else {
            Err(WikiError::invalid(
                "legacy manifest contains Revision groups",
            ))
        };
    }
    if manifest.revision_trees.is_empty()
        || manifest.revision_trees.len() > 16
        || !manifest
            .revision_trees
            .windows(2)
            .all(|w| w[0].root < w[1].root)
    {
        return Err(WikiError::invalid(
            "Revision groups must be bounded, nonempty and root sorted",
        ));
    }
    if manifest.operations.len() != manifest.revision_trees.len() * 4
        || manifest.origin.is_some()
        || manifest.inverse_of.is_some()
    {
        return Err(WikiError::invalid(
            "Revision groups require only complete bounded refresh operations",
        ));
    }
    // Retained input is hostile even when a caller did not supply a read budget.
    // Bound committed bytes before the executor can allocate a whole group's
    // independently owned payloads. Primitive admission additionally counts its
    // workspace; preparation's narrower ordinary-writer budgets are unchanged.
    let declared_bytes = manifest
        .operations
        .iter()
        .try_fold(0u64, |total, operation| {
            operation
                .before_payload
                .iter()
                .chain(operation.after_payload.iter())
                .try_fold(total, |total, payload| total.checked_add(payload.byte_len))
        });
    if declared_bytes.is_none_or(|total| total > MAX_GRAPH_INPUT_BYTES as u64) {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "complete Revision refresh declared payload bytes exceed aggregate bound",
        ));
    }
    let mut sources = BTreeSet::new();
    let mut covered = BTreeSet::new();
    for group in &manifest.revision_trees {
        let parts: Vec<_> = group.root.as_str().split('/').collect();
        if parts.len() != 4
            || parts[0] != "sources"
            || parts[2] != "revisions"
            || crate::domain::RecordId::new(parts[1]).is_err()
            || crate::domain::RecordId::new(parts[3]).is_err()
            || group.members.len() != 3
            || !group.members.windows(2).all(|w| w[0] < w[1])
        {
            return Err(WikiError::invalid(
                "invalid exact Revision group root/membership",
            ));
        }
        if !sources.insert(parts[1]) {
            return Err(WikiError::invalid(
                "refresh contains multiple new Revisions for one Source",
            ));
        }
        let head_path = format!("sources/{}/source.md", parts[1]);
        let head = manifest
            .operations
            .iter()
            .find(|op| op.target.as_str() == head_path)
            .ok_or_else(|| WikiError::invalid("Revision group lacks Source head operation"))?;
        if head.role != OperationRole::MutableRecord
            || !matches!(head.before, ExpectedState::Hash(_))
            || !matches!(head.after, ExpectedState::Hash(_))
            || head.after_payload.is_none()
            || !group
                .members
                .iter()
                .all(|member| head.apply_after.contains(member))
        {
            return Err(WikiError::invalid(
                "Revision Source head shape/prerequisites differ",
            ));
        }
        let mut names = BTreeSet::new();
        for &index in &group.members {
            let op = manifest
                .operations
                .get(index)
                .ok_or_else(|| WikiError::invalid("Revision member out of range"))?;
            if !covered.insert(index)
                || op.role != OperationRole::ImmutableAsset
                || op.before != ExpectedState::Absent
                || op.before_payload.is_some()
                || !matches!(op.after, ExpectedState::Hash(_))
                || op.after_payload.is_none()
                || super::immutable::tree_path(&op.target)?.as_ref() != Some(&group.root)
            {
                return Err(WikiError::invalid(
                    "Revision group is not disjoint immutable creates",
                ));
            }
            let prefix = format!("{}/", group.root);
            let name = op
                .target
                .as_str()
                .strip_prefix(&prefix)
                .ok_or_else(|| WikiError::invalid("Revision member outside root"))?;
            names.insert(name);
        }
        if names != BTreeSet::from(["content.md", "original.bin", "revision.md"]) {
            return Err(WikiError::invalid(
                "Revision group does not contain the complete three assets",
            ));
        }
    }
    for (index, op) in manifest.operations.iter().enumerate() {
        if (op.role == OperationRole::ImmutableAsset
            || super::immutable::tree_path(&op.target)?.is_some())
            && !covered.contains(&index)
        {
            return Err(WikiError::invalid("ungrouped immutable Revision member"));
        }
    }
    execution_order(manifest)?;
    Ok(())
}

pub(crate) fn validate_operation_payload(
    manifest: &ChangeManifest,
    index: usize,
    after: Option<&[u8]>,
) -> Result<()> {
    if manifest.version != 3 {
        return Ok(());
    }
    let op = manifest
        .operations
        .get(index)
        .ok_or_else(|| WikiError::invalid("operation outside manifest"))?;
    let data =
        after.ok_or_else(|| WikiError::invalid("missing complete Revision refresh payload"))?;
    if op.after != ExpectedState::Hash(Blake3Hash::digest(data)) {
        return Err(WikiError::invalid("Revision refresh payload hash mismatch"));
    }
    if let Some(group_index) = grouped(manifest, index) {
        let group = &manifest.revision_trees[group_index];
        if !op.target.as_str().ends_with("/revision.md") {
            std::str::from_utf8(data)
                .map_err(|_| WikiError::invalid("Revision asset is not UTF-8"))?;
            return Ok(());
        }
        let note = crate::records::parse_note(data);
        let record = note
            .canonical
            .ok_or_else(|| WikiError::invalid("invalid canonical Revision payload"))?;
        let parts: Vec<_> = group.root.as_str().split('/').collect();
        if record.kind() != RecordKind::Revision
            || record.id().as_str() != parts[3]
            || record.string("wiki_source_id") != Some(parts[1])
            || record.string("wiki_source")
                != Some(format!("[[sources/{}/source.md]]", parts[1]).as_str())
            || record.string("wiki_extraction_status") != Some("complete")
            || record.string("wiki_original_path") != Some("original.bin")
            || record.string("wiki_content_path") != Some("content.md")
            || !matches!(
                record.string("wiki_media_type"),
                Some("text/plain" | "text/markdown")
            )
            || record.string("wiki_extractor") != Some("utf8-preserve-v1")
            || record.string("wiki_extractor_fingerprint")
                != Some(Blake3Hash::digest(b"utf8-preserve-v1").as_str())
        {
            return Err(WikiError::invalid(
                "Revision group payload semantics differ",
            ));
        }
        let mut hashes = Vec::new();
        for (name, field) in [
            ("original.bin", "wiki_original_hash"),
            ("content.md", "wiki_content_hash"),
        ] {
            let asset = group
                .members
                .iter()
                .map(|&member| &manifest.operations[member])
                .find(|asset| asset.target.as_str() == format!("{}/{name}", group.root))
                .ok_or_else(|| WikiError::invalid("missing Revision asset"))?;
            let ExpectedState::Hash(hash) = &asset.after else {
                return Err(WikiError::invalid("Revision asset missing hash"));
            };
            if record.string(field) != Some(hash.as_str()) {
                return Err(WikiError::invalid("Revision record and asset hash differ"));
            }
            hashes.push(hash);
        }
        if hashes[0] != hashes[1] {
            return Err(WikiError::invalid(
                "UTF-8 preserve Revision must retain exact original/content bytes",
            ));
        }
    } else {
        let head = crate::records::parse_note(data)
            .canonical
            .ok_or_else(|| WikiError::invalid("invalid Source head payload"))?;
        if head.kind() != RecordKind::Source || !matches!(op.before, ExpectedState::Hash(_)) {
            return Err(WikiError::invalid(
                "Revision refresh requires existing mutable Source head",
            ));
        }
        let root = format!(
            "sources/{}/revisions/{}",
            head.id(),
            head.string("wiki_current_revision")
                .ok_or_else(|| WikiError::invalid("Source head missing current Revision"))?
        );
        let group = manifest
            .revision_trees
            .iter()
            .find(|group| group.root.as_str() == root)
            .ok_or_else(|| WikiError::invalid("head does not name a grouped Revision"))?;
        if op.target.as_str() != format!("sources/{}/source.md", head.id())
            || head.string("wiki_revision")
                != Some(format!("[[{}/revision.md]]", group.root).as_str())
            || !group
                .members
                .iter()
                .all(|member| op.apply_after.contains(member))
        {
            return Err(WikiError::invalid(
                "Source head path or Revision prerequisites differ",
            ));
        }
    }
    Ok(())
}

pub(crate) fn try_group_refresh(manifest: &mut ChangeManifest, plan: &ChangePlan) -> Result<bool> {
    if manifest.version != 2
        || !manifest.revision_trees.is_empty()
        || manifest.origin.is_some()
        || manifest.inverse_of.is_some()
    {
        return Ok(false);
    }
    let mut candidate = manifest.clone();
    let mut groups = BTreeMap::<VaultRelativePath, Vec<usize>>::new();
    for (index, op) in manifest.operations.iter().enumerate() {
        if let Some(root) = super::immutable::tree_path(&op.target)? {
            groups.entry(root).or_default().push(index);
        }
    }
    candidate.revision_trees = groups
        .into_iter()
        .map(|(root, members)| RevisionTreeGroup { root, members })
        .collect();
    candidate.version = 3;
    if validate_structure(&candidate).is_err()
        || candidate.operations.len() != candidate.revision_trees.len() * 4
    {
        return Ok(false);
    }
    let mut seen = BTreeSet::new();
    for op in &plan.operations {
        let Some(data) = &op.proposed else {
            return Ok(false);
        };
        let Some((index, committed)) = candidate
            .operations
            .iter()
            .enumerate()
            .find(|(_, member)| member.target == op.target)
        else {
            return Ok(false);
        };
        if committed.after != ExpectedState::Hash(Blake3Hash::digest(data))
            || !seen.insert(op.target.clone())
        {
            return Ok(false);
        }
        if validate_operation_payload(&candidate, index, Some(data)).is_err() {
            return Ok(false);
        }
    }
    if seen.len() != candidate.operations.len() {
        return Ok(false);
    }
    *manifest = candidate;
    Ok(true)
}

pub(crate) fn historical_intent(state: &JournalState, group: usize) -> bool {
    state
        .frames
        .iter()
        .any(|frame| frame.event == ChangeEvent::TreeIntent { group })
}

/// Whole-root state is stronger than each member independently being before/after.
pub(crate) fn observe_group(fs: &VaultFs, manifest: &ChangeManifest, group: usize) -> Result<bool> {
    let group = &manifest.revision_trees[group];
    let root = fs.root().resolve(&group.root)?;
    match std::fs::symlink_metadata(&root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(WikiError::invalid(error.to_string())),
        Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
            return Err(tree_conflict());
        }
        Ok(_) => {}
    }
    let mut entries = 0;
    for entry in std::fs::read_dir(&root).map_err(|_| tree_conflict())? {
        entry.map_err(|_| tree_conflict())?;
        entries += 1;
        if entries > group.members.len() {
            return Err(tree_conflict());
        }
    }
    if entries != group.members.len() {
        return Err(tree_conflict());
    }
    for &member in &group.members {
        let op = &manifest.operations[member];
        let resolved = fs.root().resolve(&op.target)?;
        let metadata = std::fs::symlink_metadata(resolved).map_err(|_| tree_conflict())?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(tree_conflict());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.nlink() != 1 {
                return Err(tree_conflict());
            }
        }
        let data = read_bounded(fs, &op.target, MAX_PAYLOAD_BYTES)?.ok_or_else(tree_conflict)?;
        if op.after != ExpectedState::Hash(Blake3Hash::digest(data)) {
            return Err(tree_conflict());
        }
    }
    Ok(true)
}
fn tree_conflict() -> WikiError {
    WikiError::new(
        ErrorCode::ContentConflict,
        "Revision root is partial, unfamiliar, aliased or changed; preserved",
    )
}

pub(crate) fn require_observations(
    fs: &VaultFs,
    manifest: &ChangeManifest,
    state: &JournalState,
    abandon: bool,
) -> Result<()> {
    for index in 0..manifest.revision_trees.len() {
        if observe_group(fs, manifest, index)? && (abandon || !historical_intent(state, index)) {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "existing complete Revision requires its own historical durable TreeIntent; abandon requires the entire root absent",
            ));
        }
    }
    Ok(())
}

pub(crate) fn require_abandon_trees_absent(fs: &VaultFs, manifest: &ChangeManifest) -> Result<()> {
    for group in &manifest.revision_trees {
        match std::fs::symlink_metadata(fs.root().resolve(&group.root)?) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(WikiError::invalid(error.to_string())),
            Ok(_) => return Err(tree_conflict()),
        }
    }
    Ok(())
}

#[cfg(any(test, feature = "experimental-revision-tree-install"))]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Intent {
    File(usize),
    Tree(usize),
}

impl ChangeEngine {
    #[cfg(any(test, feature = "experimental-revision-tree-install"))]
    pub(super) fn apply_complete_revision_units(
        &self,
        permit: &crate::vault::WriterPermit,
        change: &PreparedChange,
        manifest: &ChangeManifest,
        initial: &JournalState,
        guard: &dyn Fn(bool) -> Result<()>,
    ) -> Result<JournalState> {
        use super::journal;
        require_execution(&self.fs, manifest)?;
        if let Err(error) = require_observations(&self.fs, manifest, initial, false) {
            return self.revision_failure(permit, manifest, &change.manifest_hash, initial, error);
        }
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
        let last_epoch = state.frames.iter().rposition(|f| {
            matches!(
                f.event,
                ChangeEvent::Applying
                    | ChangeEvent::ResolutionAccepted {
                        mode: ConflictResolutionMode::Resume,
                        ..
                    }
            )
        });
        let reverted = if let Some(epoch) = last_epoch {
            state.frames[epoch..].iter().any(|f| match f.event {
                ChangeEvent::Done { op } => self
                    .target_state(&manifest.operations[op].target)
                    .is_ok_and(|seen| seen == manifest.operations[op].before),
                ChangeEvent::TreeDone { group } => {
                    observe_group(&self.fs, manifest, group).is_ok_and(|present| !present)
                }
                _ => false,
            })
        } else {
            false
        };
        if reverted {
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
                .ok_or_else(|| WikiError::invalid("missing application epoch"))?;
            let mut completed = BTreeSet::new();
            let mut intent = None;
            for frame in &state.frames[epoch..] {
                match frame.event {
                    ChangeEvent::Intent { op } => intent = Some(Intent::File(op)),
                    ChangeEvent::TreeIntent { group } => intent = Some(Intent::Tree(group)),
                    ChangeEvent::Done { op } => {
                        completed.insert(op);
                        intent = None;
                    }
                    ChangeEvent::TreeDone { group } => {
                        completed.extend(manifest.revision_trees[group].members.iter().copied());
                        intent = None;
                    }
                    _ => {}
                }
            }
            for index in execution_order(manifest)? {
                self.verify_read_preconditions(permit, manifest, &change.manifest_hash)?;
                guard(false)?;
                let group_index = grouped(manifest, index);
                let unit = group_index.map(Intent::Tree).unwrap_or(Intent::File(index));
                let already_done = completed.contains(&index);
                if !already_done && intent.is_some() && intent != Some(unit) {
                    return Err(WikiError::invalid(
                        "outstanding intent contradicts contracted application order",
                    ));
                }
                if let Some(group_index) = group_index {
                    let group = &manifest.revision_trees[group_index];
                    let present = observe_group(&self.fs, manifest, group_index)?;
                    if present && !historical_intent(&state, group_index) {
                        return self.conflict(
                            permit,
                            manifest,
                            &change.manifest_hash,
                            "complete tree without durable intent",
                            self.observe(manifest)?,
                        );
                    }
                    let expected = group
                        .members
                        .iter()
                        .map(|&member| {
                            let op = &manifest.operations[member];
                            let ExpectedState::Hash(hash) = &op.after else {
                                unreachable!("validated group")
                            };
                            (op.target.clone(), hash.clone())
                        })
                        .collect::<Vec<_>>();
                    let staged = if !present {
                        let mut members = Vec::new();
                        for &member in &group.members {
                            let op = &manifest.operations[member];
                            let bytes = self
                                .verify_payload(
                                    &manifest.change_id,
                                    member,
                                    "proposed",
                                    &op.target,
                                    &op.after,
                                    &op.after_payload,
                                )?
                                .ok_or_else(|| WikiError::invalid("missing tree member payload"))?;
                            // Match pre-retention admission's exact payload capacity.
                            members.push((op.target.clone(), bytes.into_boxed_slice().into_vec()));
                        }
                        Some(self.fs.stage_revision_tree(&group.root, &members, permit)?)
                    } else {
                        None
                    };
                    if !already_done && intent != Some(unit) {
                        state = journal::append_event(
                            &self.fs,
                            permit,
                            manifest,
                            &change.manifest_hash,
                            ChangeEvent::TreeIntent { group: group_index },
                        )?;
                    }
                    guard(false)?;
                    self.verify_read_preconditions(permit, manifest, &change.manifest_hash)?;
                    let sync = match staged {
                        Some(staged) => self.fs.install_revision_tree(staged, permit),
                        None => self.fs.sync_revision_tree(&group.root, &expected, permit),
                    };
                    match sync {
                        Ok(sync) => journal::require_sync(sync)?,
                        Err(error) if error.code == ErrorCode::ContentConflict => {
                            return self.conflict(
                                permit,
                                manifest,
                                &change.manifest_hash,
                                "complete tree mutation",
                                self.observe(manifest)?,
                            );
                        }
                        Err(error) => return Err(error),
                    }
                    guard(false)?;
                    self.verify_read_preconditions(permit, manifest, &change.manifest_hash)?;
                    if !observe_group(&self.fs, manifest, group_index)? {
                        return Err(tree_conflict());
                    }
                    if !already_done {
                        state = journal::append_event(
                            &self.fs,
                            permit,
                            manifest,
                            &change.manifest_hash,
                            ChangeEvent::TreeDone { group: group_index },
                        )?;
                        intent = None;
                    }
                } else {
                    let op = &manifest.operations[index];
                    let observed = self.target_state(&op.target)?;
                    if observed != op.before && observed != op.after {
                        return self.conflict(
                            permit,
                            manifest,
                            &change.manifest_hash,
                            "operation recheck",
                            self.observe(manifest)?,
                        );
                    }
                    let staged = if observed == op.before {
                        let bytes = self
                            .verify_payload(
                                &manifest.change_id,
                                index,
                                "proposed",
                                &op.target,
                                &op.after,
                                &op.after_payload,
                            )?
                            .ok_or_else(|| WikiError::invalid("missing Source head payload"))?;
                        Some(self.fs.stage(&op.target, &bytes, permit)?)
                    } else {
                        None
                    };
                    if !already_done && intent != Some(unit) {
                        state = journal::append_event(
                            &self.fs,
                            permit,
                            manifest,
                            &change.manifest_hash,
                            ChangeEvent::Intent { op: index },
                        )?;
                    }
                    guard(false)?;
                    self.verify_read_preconditions(permit, manifest, &change.manifest_hash)?;
                    let sync = match staged {
                        Some(staged) => self.fs.replace(staged, &op.before, permit),
                        None => self.fs.sync_target(&op.target, permit),
                    };
                    match sync {
                        Ok(sync) => journal::require_sync(sync)?,
                        Err(error) if error.code == ErrorCode::ContentConflict => {
                            return self.conflict(
                                permit,
                                manifest,
                                &change.manifest_hash,
                                "guarded head mutation",
                                self.observe(manifest)?,
                            );
                        }
                        Err(error) => return Err(error),
                    }
                    guard(false)?;
                    self.verify_read_preconditions(permit, manifest, &change.manifest_hash)?;
                    if self.target_state(&op.target)? != op.after {
                        return self.conflict(
                            permit,
                            manifest,
                            &change.manifest_hash,
                            "post head mutation",
                            self.observe(manifest)?,
                        );
                    }
                    if !already_done {
                        state = journal::append_event(
                            &self.fs,
                            permit,
                            manifest,
                            &change.manifest_hash,
                            ChangeEvent::Done { op: index },
                        )?;
                        intent = None;
                    }
                }
            }
            guard(true)?;
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
            return Err(super::apply::recovery_error(
                "complete Revision Change cannot publish in current state",
            ));
        }
        Ok(state)
    }
}

#[cfg(not(any(test, feature = "experimental-revision-tree-install")))]
impl ChangeEngine {
    pub(super) fn apply_complete_revision_units(
        &self,
        _permit: &crate::vault::WriterPermit,
        _change: &PreparedChange,
        manifest: &ChangeManifest,
        _initial: &JournalState,
        _guard: &dyn Fn(bool) -> Result<()>,
    ) -> Result<JournalState> {
        require_execution(&self.fs, manifest)?;
        Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "complete Revision executor is not compiled",
        ))
    }
}

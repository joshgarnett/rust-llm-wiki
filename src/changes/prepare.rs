//! Exact retained proposals. This module grants no canonical application authority.
use super::{
    journal::{self, require_sync},
    types::*,
};
use crate::{
    domain::{Blake3Hash, ErrorCode, RecordId, RecordKind, Result, VaultRelativePath, WikiError},
    records::parse_note,
    vault::{ExpectedState, VaultFs, VaultRoot, WriterPermit},
};
use serde::{
    Deserialize,
    de::{self, DeserializeOwned, MapAccess, SeqAccess, Visitor},
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
};

pub const MAX_MANIFEST_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_JOURNAL_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const MAX_OPS: usize = 10_000;
pub(super) const MAX_GRAPH_INPUT_BYTES: usize = 64 * 1024 * 1024;
pub(super) const MAX_INVERSE_PAYLOAD_BYTES: usize = 256 * 1024 * 1024;
const OPEN: &[u8] = b"```lwiki-change-v1\n";

#[cfg(test)]
#[path = "maintenance_retention_tests.rs"]
mod maintenance_retention_tests;

/// Trusted admission result. The caller persists its exact normal manifest in
/// bounded import intent before asking the engine to retain the proposal.
pub(crate) struct NamedPreparation {
    manifest: ChangeManifest,
    manifest_hash: Blake3Hash,
    plan: ChangePlan,
}

impl NamedPreparation {
    pub(crate) fn manifest(&self) -> &ChangeManifest {
        &self.manifest
    }

    pub(crate) fn prepared(&self) -> PreparedChange {
        PreparedChange {
            change_id: self.manifest.change_id.clone(),
            manifest_hash: self.manifest_hash.clone(),
        }
    }
}

impl ChangeEngine {
    pub fn new(fs: VaultFs) -> Result<Self> {
        VaultRoot::explicit(fs.root().path())?;
        let marker = read_bounded(&fs, &VaultRelativePath::new("WIKI.md")?, MAX_MANIFEST_BYTES)?
            .ok_or_else(|| WikiError::invalid("missing WIKI.md"))?;
        let parsed = parse_note(&marker);
        let record = parsed
            .canonical
            .ok_or_else(|| WikiError::invalid("WIKI.md must be a valid vault record"))?;
        if record.kind() != RecordKind::Vault {
            return Err(WikiError::invalid("WIKI.md must be kind vault"));
        }
        Ok(Self {
            fs,
            vault_id: record.id().clone(),
        })
    }
    pub fn fs(&self) -> &VaultFs {
        &self.fs
    }
    pub fn vault_id(&self) -> &RecordId {
        &self.vault_id
    }
    pub(crate) fn require_binding(&self) -> Result<()> {
        VaultRoot::explicit(self.fs.root().path())?;
        let marker = read_bounded(
            &self.fs,
            &VaultRelativePath::new("WIKI.md")?,
            MAX_MANIFEST_BYTES,
        )?
        .ok_or_else(|| WikiError::invalid("missing WIKI.md"))?;
        let parsed = parse_note(&marker);
        if !parsed
            .canonical
            .is_some_and(|r| r.kind() == RecordKind::Vault && r.id() == &self.vault_id)
        {
            return Err(WikiError::invalid("vault identity changed"));
        }
        Ok(())
    }
    /// Read-only expected-state validation; does not create a lock or directories.
    pub fn plan(&self, draft: &ChangeDraft) -> Result<ChangePlan> {
        self.require_binding()?;
        let bounded_graph = draft.inverse_of.is_some()
            || draft.origin.as_ref().is_some_and(|origin| {
                matches!(
                    origin.operation,
                    OriginOperation::GraphDecide | OriginOperation::GraphReview
                )
            });
        if bounded_graph {
            let proposed_bytes = draft.operations.iter().try_fold(0usize, |sum, op| {
                sum.checked_add(op.proposed.as_ref().map_or(0, Vec::len))
            });
            if proposed_bytes.is_none_or(|n| n > MAX_GRAPH_INPUT_BYTES) {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "inverse proposed bytes exceed aggregate ceiling",
                ));
            }
        }
        if draft.operations.len() > MAX_OPS
            || draft.read_preconditions.len() > MAX_OPS
            || draft.title.len() > 16_384
        {
            return Err(WikiError::invalid("draft exceeds limits"));
        }
        let targets: Vec<_> = draft
            .operations
            .iter()
            .map(|op| op.target.clone())
            .collect();
        validate_targets(&self.fs, &targets)?;
        let mut read_preconditions = draft.read_preconditions.clone();
        read_preconditions.sort_by(|a, b| a.path.cmp(&b.path));
        let read_paths: Vec<_> = read_preconditions.iter().map(|r| r.path.clone()).collect();
        validate_targets(&self.fs, &read_paths)?;
        for condition in &read_preconditions {
            let bytes = read_bounded(&self.fs, &condition.path, MAX_PAYLOAD_BYTES)?;
            if state(bytes.as_deref()) != condition.expected {
                return Err(WikiError::new(
                    ErrorCode::ContentConflict,
                    format!("read precondition mismatch: {}", condition.path),
                ));
            }
        }
        let mut operations = draft.operations.clone();
        operations.sort_by(|a, b| a.target.cmp(&b.target));
        topological_order(&resolve_dependencies(&operations)?)?;
        let mut retained = Vec::new();
        let mut images = Vec::new();
        let mut roles = Vec::new();
        let mut dropped = BTreeSet::new();
        let mut inverse_reads = if bounded_graph {
            MAX_GRAPH_INPUT_BYTES
        } else {
            usize::MAX
        };
        for operation in operations {
            let before = read_with_budget(&self.fs, &operation.target, &mut inverse_reads)?;
            let actual = state(before.as_deref());
            if operation.expected != actual {
                return Err(WikiError::new(
                    ErrorCode::ContentConflict,
                    format!("expected state mismatch: {}", operation.target),
                ));
            }
            if operation
                .proposed
                .as_ref()
                .is_some_and(|b| b.len() > MAX_PAYLOAD_BYTES)
            {
                return Err(WikiError::invalid("payload exceeds limit"));
            }
            if before == operation.proposed {
                dropped.insert(operation.target);
                continue;
            }
            let role = infer_role(
                &self.fs,
                &operation.target,
                before.as_deref(),
                operation.proposed.as_deref(),
                true,
            )?;
            roles.push(role);
            images.push(before);
            retained.push(operation);
        }
        for op in &mut retained {
            op.apply_after.retain(|path| !dropped.contains(path));
        }
        let new_targets: Vec<_> = retained.iter().map(|op| op.target.clone()).collect();
        for op in &mut retained {
            for dependency in source_dependencies(op.proposed.as_deref(), &new_targets) {
                if !op.apply_after.contains(&dependency) {
                    op.apply_after.push(dependency);
                }
            }
        }
        let deps = resolve_dependencies(&retained)?;
        topological_order(&deps)?;
        let write_expectations: BTreeMap<_, _> =
            retained.iter().map(|o| (&o.target, &o.expected)).collect();
        for condition in &read_preconditions {
            if let Some(expected) = write_expectations.get(&condition.path)
                && **expected != condition.expected
            {
                return Err(WikiError::invalid(
                    "read condition disagrees with write expectation",
                ));
            }
        }
        // Guarded writes already retain the same before-state. Dropped no-ops do not.
        read_preconditions.retain(|r| !write_expectations.contains_key(&r.path));
        let combined_paths: Vec<_> = retained
            .iter()
            .map(|o| o.target.clone())
            .chain(read_preconditions.iter().map(|r| r.path.clone()))
            .collect();
        validate_targets(&self.fs, &combined_paths)?;
        Ok(ChangePlan {
            read_preconditions,
            operations: retained,
            roles,
            before: images,
        })
    }
    pub fn prepare(&self, permit: &WriterPermit, draft: ChangeDraft) -> Result<ChangeInspection> {
        if let Some(origin) = draft.origin.clone() {
            return self
                .prepare_or_reuse(permit, origin, OriginPolicy::ReuseOrConflict, || Ok(draft))
                .map(|outcome| outcome.change);
        }
        self.prepare_new(permit, draft)
    }
    fn prepare_new(&self, permit: &WriterPermit, draft: ChangeDraft) -> Result<ChangeInspection> {
        permit.require_root(self.fs.root())?;
        let plan = self.plan(&draft)?; // all expectations first, before any retained allocation
        let change_id = RecordId::generate(RecordKind::Change)?;
        let created_at = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|e| WikiError::invalid(e.to_string()))?;
        let sealed = self.seal_plan(
            draft,
            plan,
            NamedChangeIdentity {
                change_id,
                created_at,
            },
        )?;
        let manifest = sealed.manifest;
        self.retain_plan(permit, &manifest, sealed.plan)?;
        let note = render_prepared_note(&manifest, &sealed.manifest_hash)?;
        self.persist(permit, &manifest_path(&manifest.change_id)?, &note)?;
        // Reload validates retained bytes, exact fence identity and manifest before authorization.
        let loaded = self.load_manifest(&manifest.change_id)?;
        journal::append_event(
            &self.fs,
            permit,
            &loaded.0,
            &loaded.1,
            ChangeEvent::Prepared,
        )?;
        self.inspect(&manifest.change_id)
    }

    /// Runs normal admission under the writer without retaining any proposal.
    /// Import intent must durably bind the result before named retention.
    pub(crate) fn seal_named(
        &self,
        permit: &WriterPermit,
        identity: NamedChangeIdentity,
        draft: ChangeDraft,
    ) -> Result<NamedPreparation> {
        permit.require_root(self.fs.root())?;
        if draft.origin.is_some()
            || draft.inverse_of.is_some()
            || !(3..=32).contains(&draft.operations.len())
            || !(2..=16).contains(&draft.allocated_ids.len())
            || draft
                .operations
                .iter()
                .any(|operation| operation.expected != ExpectedState::Absent)
        {
            return Err(WikiError::invalid(
                "named preparation requires bounded fresh capture operations without graph origins or inverses",
            ));
        }
        time::OffsetDateTime::parse(
            &identity.created_at,
            &time::format_description::well_known::Rfc3339,
        )
        .map_err(|error| WikiError::invalid(format!("invalid named Change time: {error}")))?;
        let plan = self.plan(&draft)?;
        self.seal_plan(draft, plan, identity)
    }

    /// Separate internally validated JobBatch admission. Capture's fresh-only
    /// constraints and namespace remain unchanged.
    pub(crate) fn seal_named_job(
        &self,
        permit: &WriterPermit,
        identity: NamedChangeIdentity,
        operation: &crate::changes::indexed_refresh::IndexedWriteOperation,
        draft: ChangeDraft,
    ) -> Result<NamedPreparation> {
        permit.require_root(self.fs.root())?;
        if draft.operations.is_empty()
            || draft.operations.len() > 3
            || draft.read_preconditions.len() > 4096
        {
            return Err(WikiError::invalid(
                "named JobBatch exceeds selected admission bound",
            ));
        }
        crate::jobs::checkpoint::require_named_job_identity(
            &self.vault_id,
            operation,
            &draft,
            &identity,
        )?;
        crate::jobs::checkpoint::validate_job_payloads(
            &self.fs,
            &self.vault_id,
            operation,
            &draft,
        )?;
        time::OffsetDateTime::parse(
            &identity.created_at,
            &time::format_description::well_known::Rfc3339,
        )
        .map_err(|e| WikiError::invalid(format!("invalid named Job time: {e}")))?;
        let plan = self.plan(&draft)?;
        self.seal_plan(draft, plan, identity)
    }

    fn seal_plan(
        &self,
        draft: ChangeDraft,
        plan: ChangePlan,
        identity: NamedChangeIdentity,
    ) -> Result<NamedPreparation> {
        let version = if crate::storage::layout::active(self.fs.root())? {
            2
        } else {
            1
        };
        let deps = resolve_dependencies(&plan.operations)?;
        let mut manifest = ChangeManifest {
            version,
            vault_id: self.vault_id.clone(),
            change_id: identity.change_id,
            title: draft.title,
            created_at: identity.created_at,
            origin: draft.origin,
            inverse_of: draft.inverse_of,
            allocated_ids: draft.allocated_ids,
            read_preconditions: plan.read_preconditions.clone(),
            operations: Vec::new(),
        };
        for (index, operation) in plan.operations.iter().enumerate() {
            let reference = |side: &str, bytes: Option<&[u8]>| -> Result<Option<PayloadRef>> {
                bytes
                    .map(|bytes| {
                        let hash = Blake3Hash::digest(bytes);
                        let path = if version == 2 {
                            crate::storage::layout::object_path(&hash)?
                        } else {
                            payload_path(&manifest.change_id, index, side, &operation.target)?
                        };
                        Ok(PayloadRef {
                            path,
                            hash,
                            byte_len: bytes.len() as u64,
                        })
                    })
                    .transpose()
            };
            let before_payload = reference("before", plan.before[index].as_deref())?;
            let after_payload = reference("proposed", operation.proposed.as_deref())?;
            manifest.operations.push(ChangeOp {
                target: operation.target.clone(),
                before: operation.expected.clone(),
                after: state(operation.proposed.as_deref()),
                before_payload,
                after_payload,
                role: plan.roles[index],
                apply_after: deps[index].clone(),
            });
        }
        let json = serde_json::to_vec(&manifest).map_err(|e| WikiError::invalid(e.to_string()))?;
        if json.len() > MAX_MANIFEST_BYTES {
            return Err(WikiError::invalid("manifest exceeds limit"));
        }
        Ok(NamedPreparation {
            manifest,
            manifest_hash: Blake3Hash::digest(&json),
            plan,
        })
    }

    /// Complete only the exact initially admitted proposal named in import
    /// intent. The catalog must classify active/terminal authority first; this
    /// method grants no canonical application or stale-base replacement.
    pub(crate) fn prepare_named(
        &self,
        permit: &WriterPermit,
        expected: &ChangeManifest,
        sealed: &NamedPreparation,
    ) -> Result<ChangeInspection> {
        permit.require_root(self.fs.root())?;
        self.require_binding()?;
        if expected != sealed.manifest()
            || expected.vault_id != self.vault_id
            || expected.version
                != if crate::storage::layout::active(self.fs.root())? {
                    2
                } else {
                    1
                }
        {
            return Err(WikiError::invalid(
                "named preparation differs from frozen import intent",
            ));
        }
        for path in std::iter::once(manifest_path(&expected.change_id)?)
            .chain(std::iter::once(journal::journal_path(&expected.change_id)?))
            .chain(expected.operations.iter().flat_map(|operation| {
                [&operation.before_payload, &operation.after_payload]
                    .into_iter()
                    .filter_map(|payload| payload.as_ref().map(|payload| payload.path.clone()))
            }))
        {
            self.require_named_single_link(&path)?;
        }
        let state = journal::load_journal(&self.fs, expected, &sealed.manifest_hash)?;
        if state.status == Some(ChangeStatus::Prepared) {
            if state.torn_tail {
                return Err(super::apply::recovery_error(
                    "named Prepared attempt has an incomplete later journal frame",
                ));
            }
            let (manifest, hash) = self.load_manifest(&expected.change_id)?;
            if manifest != *expected || hash != sealed.manifest_hash {
                return Err(WikiError::invalid(
                    "retained named proposal differs from import intent",
                ));
            }
            require_sync(
                self.fs
                    .sync_target(&journal::journal_path(&expected.change_id)?, permit)?,
            )?;
            return self.inspect(&expected.change_id);
        }
        if state.status.is_some() {
            return Err(super::apply::recovery_error(
                "named attempt requires application or terminal dispatch before preparation",
            ));
        }
        let note = render_prepared_note(expected, &sealed.manifest_hash)?;
        let mut files: BTreeMap<VaultRelativePath, &[u8]> = BTreeMap::new();
        files.insert(manifest_path(&expected.change_id)?, &note);
        for (index, operation) in expected.operations.iter().enumerate() {
            for (reference, bytes) in [
                (
                    &operation.before_payload,
                    sealed.plan.before[index].as_deref(),
                ),
                (
                    &operation.after_payload,
                    sealed.plan.operations[index].proposed.as_deref(),
                ),
            ] {
                if let (Some(reference), Some(bytes)) = (reference, bytes)
                    && expected.version == 1
                    && files.insert(reference.path.clone(), bytes).is_some()
                {
                    return Err(WikiError::invalid("duplicate named retained payload path"));
                }
            }
        }
        self.require_named_prefix(expected, &files)?;
        // Keep payload-before-note ordering, including on a resumed partial.
        for (index, operation) in expected.operations.iter().enumerate() {
            for (reference, bytes) in [
                (
                    &operation.before_payload,
                    sealed.plan.before[index].as_deref(),
                ),
                (
                    &operation.after_payload,
                    sealed.plan.operations[index].proposed.as_deref(),
                ),
            ] {
                if let (Some(reference), Some(bytes)) = (reference, bytes) {
                    if expected.version == 2 {
                        crate::storage::layout::put(&self.fs, permit, &reference.path, bytes)?;
                    } else {
                        self.persist_named(permit, &reference.path, bytes)?;
                    }
                }
            }
        }
        self.persist_named(permit, &manifest_path(&expected.change_id)?, &note)?;
        let (manifest, hash) = self.load_manifest(&expected.change_id)?;
        if manifest != *expected || hash != sealed.manifest_hash {
            return Err(WikiError::invalid(
                "completed named retained proposal differs",
            ));
        }
        journal::append_event(&self.fs, permit, &manifest, &hash, ChangeEvent::Prepared)?;
        self.inspect(&expected.change_id)
    }

    fn persist_named(
        &self,
        permit: &WriterPermit,
        path: &VaultRelativePath,
        bytes: &[u8],
    ) -> Result<()> {
        self.require_named_single_link(path)?;
        match read_bounded(&self.fs, path, MAX_PAYLOAD_BYTES)? {
            None => self.persist(permit, path, bytes),
            Some(existing) if existing == bytes => require_sync(self.fs.sync_target(path, permit)?),
            Some(_) => Err(WikiError::new(
                ErrorCode::ContentConflict,
                "named retained path contains unfamiliar bytes",
            )),
        }
    }

    pub(crate) fn require_named_single_link(&self, path: &VaultRelativePath) -> Result<()> {
        let metadata = match fs::symlink_metadata(self.fs.root().resolve(path)?) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(io_error(error)),
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "named retained file has an unsafe type",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.nlink() != 1 {
                return Err(WikiError::new(
                    ErrorCode::ContentConflict,
                    "named retained file must have one physical link",
                ));
            }
        }
        #[cfg(not(unix))]
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "single-link named retention is not qualified on this platform",
        ));
        #[cfg(unix)]
        Ok(())
    }

    /// This inventory is bounded to one declared Change prefix, never history.
    /// An empty or stage-only prefix is ambiguous and must remain untouched.
    fn require_named_prefix(
        &self,
        manifest: &ChangeManifest,
        files: &BTreeMap<VaultRelativePath, &[u8]>,
    ) -> Result<()> {
        let prefix = VaultRelativePath::new(format!("changes/{}", manifest.change_id))?;
        let physical = self.fs.root().resolve(&prefix)?;
        match fs::symlink_metadata(&physical) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(io_error(error)),
            Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
                return Err(WikiError::new(
                    ErrorCode::ContentConflict,
                    "named Change prefix is unsafe",
                ));
            }
            Ok(_) => {}
        }
        let allowed_directories: BTreeSet<_> = files
            .keys()
            .flat_map(|path| {
                path.as_str()
                    .match_indices('/')
                    .map(|(end, _)| path.as_str()[..end].to_owned())
            })
            .collect();
        let mut stack = vec![prefix];
        let mut entries = 0usize;
        let mut matched = 0usize;
        while let Some(directory) = stack.pop() {
            for entry in fs::read_dir(self.fs.root().resolve(&directory)?).map_err(io_error)? {
                entries += 1;
                if entries > 128 {
                    return Err(WikiError::new(
                        ErrorCode::BudgetExceeded,
                        "named retained prefix exceeds bounded capture inventory",
                    ));
                }
                let entry = entry.map_err(io_error)?;
                let name = entry.file_name();
                let name = name
                    .to_str()
                    .ok_or_else(|| WikiError::invalid("non-UTF8 named retained entry"))?;
                let path = VaultRelativePath::new(format!("{directory}/{name}"))?;
                let kind = entry.file_type().map_err(io_error)?;
                if kind.is_dir() && allowed_directories.contains(path.as_str()) {
                    stack.push(path);
                } else if kind.is_file() && files.contains_key(&path) {
                    self.fs.validate_paths(std::slice::from_ref(&path))?;
                    let expected = files[&path];
                    let actual = read_bounded(&self.fs, &path, expected.len())?;
                    if actual.as_deref() != Some(expected) {
                        return Err(WikiError::new(
                            ErrorCode::ContentConflict,
                            "named retained prefix contains unfamiliar bytes",
                        ));
                    }
                    matched += 1;
                } else {
                    let mut error = WikiError::new(
                        ErrorCode::ContentConflict,
                        "named Change prefix is unmarked or contains unfamiliar entries",
                    );
                    error.details = serde_json::json!({"named_attempt_state":"ambiguous_prefix", "change_id":manifest.change_id});
                    return Err(error);
                }
            }
        }
        if matched == 0 {
            let mut error = WikiError::new(
                ErrorCode::ContentConflict,
                "named Change prefix is unmarked and must be preserved",
            );
            error.details = serde_json::json!({"named_attempt_state":"ambiguous_prefix", "change_id":manifest.change_id});
            return Err(error);
        }
        Ok(())
    }
    pub fn prepare_or_reuse<F>(
        &self,
        permit: &WriterPermit,
        origin: ChangeOrigin,
        policy: OriginPolicy,
        build_once: F,
    ) -> Result<PreparationOutcome>
    where
        F: FnOnce() -> Result<ChangeDraft>,
    {
        permit.require_root(self.fs.root())?;
        self.require_binding()?;
        let mut matching = None;
        let mut differing = false;
        for id in self.change_ids()? {
            let change = self.inspect_history(&id)?;
            if let Some(existing) = &change.manifest.origin
                && existing.operation == origin.operation
                && existing.packet_id == origin.packet_id
            {
                if existing.response_hash == origin.response_hash {
                    if matching.is_some() {
                        return Err(WikiError::invalid("duplicate matching origins"));
                    }
                    matching = Some(change);
                } else {
                    differing = true;
                }
            }
        }
        if let Some(change) = matching {
            return Ok(PreparationOutcome {
                change,
                reused: true,
            });
        }
        if differing && policy == OriginPolicy::ReuseOrConflict {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "different response already retained for packet",
            ));
        }
        let mut draft = build_once()?;
        if draft.origin.as_ref().is_some_and(|o| o != &origin) {
            return Err(WikiError::invalid("builder origin differs"));
        }
        draft.origin = Some(origin);
        Ok(PreparationOutcome {
            change: self.prepare_new(permit, draft)?,
            reused: false,
        })
    }
    pub fn inspect(&self, id: &RecordId) -> Result<ChangeInspection> {
        self.require_binding()?;
        let (manifest, manifest_hash) = self.load_manifest(id)?;
        let journal = journal::load_journal(&self.fs, &manifest, &manifest_hash)?;
        let terminal = super::outcome::terminal_report(&self.fs, &manifest, &manifest_hash)?;
        let status = terminal.map_or_else(
            || journal.status.unwrap_or(ChangeStatus::Prepared),
            |report| report.status,
        );
        let note = read_bounded(&self.fs, &manifest_path(id)?, MAX_MANIFEST_BYTES + 65_536)?
            .ok_or_else(|| WikiError::invalid("missing change note"))?;
        let note_status = parse_note(&note)
            .canonical
            .and_then(|record| record.string("wiki_status").map(str::to_owned))
            .ok_or_else(|| WikiError::invalid("missing note status"))?;
        let mut observations = Vec::new();
        for operation in manifest
            .operations
            .iter()
            .filter(|_| !matches!(status, ChangeStatus::Committed | ChangeStatus::Aborted))
        {
            observations.push(TargetObservation {
                target: operation.target.clone(),
                before: operation.before.clone(),
                after: operation.after.clone(),
                observed: state(
                    read_bounded(&self.fs, &operation.target, MAX_PAYLOAD_BYTES)?.as_deref(),
                ),
            });
        }
        Ok(ChangeInspection {
            prepared: PreparedChange {
                change_id: id.clone(),
                manifest_hash,
            },
            manifest,
            journal,
            status,
            observations,
            note_status,
        })
    }
    pub(crate) fn change_ids(&self) -> Result<Vec<RecordId>> {
        self.classify_change_directories()
            .map(|(actionable, _)| actionable)
    }
    pub(crate) fn change_ids_checked(
        &self,
        progress: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Vec<RecordId>> {
        self.classify_change_directories_checked(progress)
            .map(|(actionable, _)| actionable)
    }
    /// Read-only diagnostics for bytes retained before a manifest became durable.
    /// These directories are preserved, and cannot authorize an application.
    pub fn incomplete_preparations(&self) -> Result<Vec<RecordId>> {
        self.require_binding()?;
        self.classify_change_directories()
            .map(|(_, incomplete)| incomplete)
    }
    fn classify_change_directories(&self) -> Result<(Vec<RecordId>, Vec<RecordId>)> {
        self.classify_change_directories_checked(&mut || Ok(()))
    }
    fn classify_change_directories_checked(
        &self,
        progress: &mut dyn FnMut() -> Result<()>,
    ) -> Result<(Vec<RecordId>, Vec<RecordId>)> {
        progress()?;
        let relative = VaultRelativePath::new("changes")?;
        let path = self.fs.root().resolve(&relative)?;
        let entries = match fs::read_dir(path) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok((Vec::new(), Vec::new()));
            }
            Err(e) => return Err(io_error(e)),
        };
        let mut ids = Vec::new();
        let mut incomplete = Vec::new();
        for entry in entries {
            progress()?;
            let entry = entry.map_err(io_error)?;
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| WikiError::invalid("non UTF8 change directory"))?;
            let id = RecordId::new(name)?;
            let kind = entry.file_type().map_err(io_error)?;
            if kind.is_symlink() || !kind.is_dir() {
                return Err(WikiError::invalid("invalid retained change entry"));
            }
            if read_bounded(&self.fs, &manifest_path(&id)?, MAX_MANIFEST_BYTES + 65_536)?.is_some()
            {
                // Existing malformed notes remain actionable errors on normal loading.
                ids.push(id);
                continue;
            }
            for evidence in [
                journal::journal_path(&id)?,
                VaultRelativePath::new(format!("changes/{id}/outcome.json"))?,
                VaultRelativePath::new(format!("changes/{id}/validation.json"))?,
                VaultRelativePath::new(format!("changes/{id}/revision-trees.json"))?,
            ] {
                progress()?;
                if read_bounded(&self.fs, &evidence, MAX_JOURNAL_BYTES)?.is_some() {
                    return Err(WikiError::new(
                        ErrorCode::RecoveryRequired,
                        format!(
                            "retained change {id} is missing its manifest but has durable evidence: {evidence}"
                        ),
                    ));
                }
            }
            incomplete.push(id);
        }
        ids.sort();
        incomplete.sort();
        progress()?;
        Ok((ids, incomplete))
    }
    pub(crate) fn load_manifest(&self, id: &RecordId) -> Result<(ChangeManifest, Blake3Hash)> {
        let mut remaining = usize::MAX;
        self.load_manifest_with_budget(id, &mut remaining)
    }
    /// Bound aggregate retained payload reads before validating/allocating any payload.
    pub(crate) fn load_manifest_with_budget(
        &self,
        id: &RecordId,
        remaining: &mut usize,
    ) -> Result<(ChangeManifest, Blake3Hash)> {
        let (manifest, hash) = self.load_manifest_structure(id)?;
        let payload_bytes = manifest
            .operations
            .iter()
            .flat_map(|op| [&op.before_payload, &op.after_payload])
            .flatten()
            .try_fold(0usize, |total, payload| {
                usize::try_from(payload.byte_len)
                    .ok()
                    .and_then(|len| total.checked_add(len))
                    .ok_or_else(|| {
                        WikiError::new(
                            ErrorCode::BudgetExceeded,
                            "retained payload aggregate length overflow",
                        )
                    })
            })?;
        let intrinsic_limit = if manifest.inverse_of.is_some() {
            MAX_INVERSE_PAYLOAD_BYTES
        } else if manifest.origin.as_ref().is_some_and(|origin| {
            matches!(
                origin.operation,
                OriginOperation::GraphDecide | OriginOperation::GraphReview
            )
        }) {
            128 * 1024 * 1024
        } else {
            usize::MAX
        };
        if payload_bytes > (*remaining).min(intrinsic_limit) {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "retained inverse ancestry exceeds payload read ceiling",
            ));
        }
        *remaining -= payload_bytes;
        self.validate_manifest(&manifest, id)?;
        Ok((manifest, hash))
    }
    /// Read exact manifest identity and structural bindings only. This is history
    /// evidence, not permission to apply or invert without retained payloads.
    pub(crate) fn load_manifest_structure(
        &self,
        id: &RecordId,
    ) -> Result<(ChangeManifest, Blake3Hash)> {
        let bytes = read_bounded(&self.fs, &manifest_path(id)?, MAX_MANIFEST_BYTES + 65_536)?
            .ok_or_else(|| WikiError::invalid("missing retained manifest"))?;
        let parsed = parse_note(&bytes);
        let record = parsed
            .canonical
            .as_ref()
            .ok_or_else(|| WikiError::invalid("invalid change note envelope"))?;
        if record.kind() != RecordKind::Change || record.id() != id {
            return Err(WikiError::invalid("change envelope identity mismatch"));
        }
        let body = parsed.body();
        let opens: Vec<_> = body
            .windows(OPEN.len())
            .enumerate()
            .filter(|(_, w)| *w == OPEN)
            .map(|(i, _)| i)
            .collect();
        if opens.len() != 1 {
            return Err(WikiError::invalid("manifest requires one exact fence"));
        }
        let start = opens[0];
        if start != 0 && body[start - 1] != b'\n' {
            return Err(WikiError::invalid("manifest fence must start a line"));
        }
        let tail = &body[start + OPEN.len()..];
        let end = tail
            .windows(4)
            .position(|w| w == b"\n```")
            .ok_or_else(|| WikiError::invalid("unclosed manifest fence"))?;
        if tail
            .get(end + 4)
            .is_some_and(|c| *c != b'\n' && *c != b'\r')
        {
            return Err(WikiError::invalid("invalid manifest fence close"));
        }
        let json = &tail[..end];
        if json.len() > MAX_MANIFEST_BYTES {
            return Err(WikiError::invalid("manifest exceeds limit"));
        }
        let hash = Blake3Hash::digest(json);
        if record.string("wiki_manifest_hash") != Some(hash.as_str()) {
            return Err(WikiError::invalid("manifest fence hash mismatch"));
        }
        let manifest: ChangeManifest = strict_json(json)?;
        self.validate_manifest_structure(&manifest, id)?;
        Ok((manifest, hash))
    }
    pub(crate) fn validate_manifest(&self, manifest: &ChangeManifest, id: &RecordId) -> Result<()> {
        self.validate_manifest_structure(manifest, id)?;
        let targets: Vec<_> = manifest
            .operations
            .iter()
            .map(|op| op.target.clone())
            .collect();
        for (index, operation) in manifest.operations.iter().enumerate() {
            if operation.before == operation.after {
                return Err(WikiError::invalid("retained no-op"));
            }
            let before = self.verify_payload(
                id,
                index,
                "before",
                &operation.target,
                &operation.before,
                &operation.before_payload,
            )?;
            let after = self.verify_payload(
                id,
                index,
                "proposed",
                &operation.target,
                &operation.after,
                &operation.after_payload,
            )?;
            for dependency in source_dependencies(after.as_deref(), &targets) {
                let required = targets
                    .iter()
                    .position(|p| p == &dependency)
                    .expect("in supplied targets");
                if !operation.apply_after.contains(&required) {
                    return Err(WikiError::invalid(
                        "source head lacks revision asset prerequisite",
                    ));
                }
            }
            let role = infer_role(
                &self.fs,
                &operation.target,
                before.as_deref(),
                after.as_deref(),
                false,
            )?;
            if role != operation.role
                || role == OperationRole::ImmutableAsset
                    && operation.before != ExpectedState::Absent
            {
                return Err(WikiError::invalid("invalid immutable operation role"));
            }
        }
        Ok(())
    }
    pub(super) fn validate_manifest_structure(
        &self,
        manifest: &ChangeManifest,
        id: &RecordId,
    ) -> Result<()> {
        if !matches!(manifest.version, 1 | 2)
            || &manifest.change_id != id
            || manifest.vault_id != self.vault_id
            || manifest.operations.len() > MAX_OPS
            || manifest.read_preconditions.len() > MAX_OPS
            || manifest.title.len() > 16_384
        {
            return Err(WikiError::invalid(
                "invalid manifest identity/version/limits",
            ));
        }
        let targets: Vec<_> = manifest
            .operations
            .iter()
            .map(|op| op.target.clone())
            .collect();
        validate_retained_targets(&targets)?;
        let read_paths: Vec<_> = manifest
            .read_preconditions
            .iter()
            .map(|r| r.path.clone())
            .collect();
        validate_retained_targets(&read_paths)?;
        let target_set: BTreeSet<_> = targets.iter().collect();
        if read_paths.windows(2).any(|pair| pair[0] >= pair[1])
            || read_paths.iter().any(|path| target_set.contains(path))
        {
            return Err(WikiError::invalid(
                "read conditions must be sorted, unique and unmodified",
            ));
        }
        let combined_paths: Vec<_> = targets.iter().chain(&read_paths).cloned().collect();
        validate_retained_targets(&combined_paths)?;
        if targets.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(WikiError::invalid("manifest targets must be sorted"));
        }
        let dependencies: Vec<_> = manifest
            .operations
            .iter()
            .map(|op| op.apply_after.clone())
            .collect();
        topological_order(&dependencies)?;
        for (index, op) in manifest.operations.iter().enumerate() {
            if op.before == op.after
                || op.role == OperationRole::ImmutableAsset && op.before != ExpectedState::Absent
            {
                return Err(WikiError::invalid("invalid retained operation structure"));
            }
            for (side, expected, payload) in [
                ("before", &op.before, &op.before_payload),
                ("proposed", &op.after, &op.after_payload),
            ] {
                match (expected, payload) {
                    (ExpectedState::Absent, None) => {}
                    (ExpectedState::Hash(hash), Some(payload))
                        if payload.path
                            == if manifest.version == 2 {
                                crate::storage::layout::object_path(hash)?
                            } else {
                                payload_path(id, index, side, &op.target)?
                            }
                            && &payload.hash == hash
                            && payload.byte_len <= MAX_PAYLOAD_BYTES as u64 => {}
                    _ => {
                        return Err(WikiError::invalid(
                            "invalid retained payload reference structure",
                        ));
                    }
                }
            }
        }
        Ok(())
    }
    pub(crate) fn verify_payload(
        &self,
        id: &RecordId,
        index: usize,
        side: &str,
        target: &VaultRelativePath,
        expected: &ExpectedState,
        payload: &Option<PayloadRef>,
    ) -> Result<Option<Vec<u8>>> {
        self.verify_payload_with_limit(
            id,
            index,
            side,
            target,
            (expected, payload),
            MAX_PAYLOAD_BYTES,
        )
    }
    pub(crate) fn verify_payload_with_limit(
        &self,
        id: &RecordId,
        index: usize,
        side: &str,
        target: &VaultRelativePath,
        expected_payload: (&ExpectedState, &Option<PayloadRef>),
        limit: usize,
    ) -> Result<Option<Vec<u8>>> {
        let (expected, payload) = expected_payload;
        match (expected, payload) {
            (ExpectedState::Absent, None) => Ok(None),
            (ExpectedState::Hash(hash), Some(payload)) => {
                if (payload.path != payload_path(id, index, side, target)?
                    && payload.path != crate::storage::layout::object_path(hash)?)
                    || &payload.hash != hash
                    || payload.byte_len > MAX_PAYLOAD_BYTES as u64
                {
                    return Err(WikiError::invalid("invalid retained payload reference"));
                }
                let bytes = read_bounded(&self.fs, &payload.path, limit.min(MAX_PAYLOAD_BYTES))?
                    .ok_or_else(|| WikiError::invalid("missing retained payload"))?;
                if bytes.len() as u64 != payload.byte_len
                    || Blake3Hash::digest(&bytes) != payload.hash
                {
                    return Err(WikiError::invalid("retained payload integrity failure"));
                }
                Ok(Some(bytes))
            }
            _ => Err(WikiError::invalid("payload/expectation mismatch")),
        }
    }
    /// Deduplicate exact destinations before allocation. Only independent temporary
    /// writes run in workers; destination installation/durability stays ordered.
    fn retain_plan(
        &self,
        permit: &WriterPermit,
        manifest: &ChangeManifest,
        mut plan: ChangePlan,
    ) -> Result<()> {
        let retained_layout = crate::storage::layout::active(self.fs.root())?;
        if retained_layout != (manifest.version == 2) {
            return Err(WikiError::invalid(
                "retained payload route changed during preparation",
            ));
        }
        let mut unique = Vec::<(PayloadRef, Vec<u8>)>::new();
        let mut destinations = BTreeMap::<VaultRelativePath, usize>::new();
        for (index, operation) in plan.operations.iter_mut().enumerate() {
            for (side, bytes, reference) in [
                (
                    "before",
                    plan.before[index].take(),
                    &manifest.operations[index].before_payload,
                ),
                (
                    "proposed",
                    operation.proposed.take(),
                    &manifest.operations[index].after_payload,
                ),
            ] {
                let Some(bytes) = bytes else {
                    if reference.is_some() {
                        return Err(WikiError::invalid(
                            "retained payload route changed during preparation",
                        ));
                    }
                    continue;
                };
                let hash = Blake3Hash::digest(&bytes);
                let path = if retained_layout {
                    crate::storage::layout::object_path(&hash)?
                } else {
                    payload_path(&manifest.change_id, index, side, &operation.target)?
                };
                let actual = PayloadRef {
                    path: path.clone(),
                    hash,
                    byte_len: bytes.len() as u64,
                };
                if reference.as_ref() != Some(&actual) {
                    return Err(WikiError::invalid(
                        "retained payload route changed during preparation",
                    ));
                }
                if let Some(previous) = destinations.get(&path) {
                    if unique[*previous].1 != bytes {
                        return Err(WikiError::new(
                            ErrorCode::ContentConflict,
                            "retained destination has conflicting payload bytes",
                        ));
                    }
                } else {
                    destinations.insert(path, unique.len());
                    unique.push((actual, bytes));
                }
            }
        }
        // Plan bytes retain their existing admission; these job reservations also
        // cover owned payloads, immutable authentication buffers and descriptors.
        // Consume one bounded chunk before admitting any later chunk.
        let proof = if unique.is_empty() {
            None
        } else {
            crate::storage::maintenance_activation::prepare_parallel_activation(permit, &self.fs)?
        };
        let mut pending = unique.into_iter().peekable();
        while pending.peek().is_some() {
            let mut jobs = Vec::with_capacity(crate::maintenance_parallel::MAX_JOBS);
            let mut paths = Vec::with_capacity(crate::maintenance_parallel::MAX_JOBS);
            let mut reserved = (jobs.capacity()
                * std::mem::size_of::<
                    crate::maintenance_parallel::Job<Option<crate::vault::StagedFile>>,
                >()
                + paths.capacity() * std::mem::size_of::<VaultRelativePath>())
                as u64;
            let mut eligible = proof.is_some();
            while jobs.len() < crate::maintenance_parallel::MAX_JOBS {
                let Some((reference, bytes)) = pending.peek() else {
                    break;
                };
                let path_workspace =
                    crate::vault::paths::parallel_path_workspace(self.fs.root(), &reference.path);
                let workspace = path_workspace.unwrap_or(0).max(
                    proof
                        .as_ref()
                        .and_then(|proof| proof.path_workspace())
                        .unwrap_or(0),
                );
                // Route/result capacities are bounded below before creating any
                // temporary file. Reserve enough bookkeeping to keep one pending
                // descriptor outside this joined batch until admission is known.
                let capture_bytes = std::mem::size_of::<(
                    VaultFs,
                    crate::vault::fs::StagePreparation,
                    PayloadRef,
                    Vec<u8>,
                    bool,
                )>();
                let (parent, _) = reference
                    .path
                    .as_str()
                    .rsplit_once('/')
                    .expect("retained payload has a parent");
                require_sync(
                    self.fs
                        .ensure_directory(&VaultRelativePath::new(parent)?, permit)?,
                )?;
                let prepared = self.fs.prepare_stage(&reference.path, permit)?;
                let fs = self.fs.clone();
                let result_bound = prepared.result_owned_bound();
                let reservation = bytes.capacity() as u64
                    + bytes.len() as u64
                    + crate::storage::maintenance_activation::WORKSPACE_BYTES
                    + 64 * 1024
                    + workspace
                    + fs.owned_bytes() as u64
                    + prepared.owned_bytes() as u64
                    + reference.path.owned_capacity() as u64 * 2
                    + reference.hash.owned_capacity() as u64
                    + std::mem::size_of::<PayloadRef>() as u64
                    + result_bound
                    + std::mem::size_of::<Option<crate::vault::StagedFile>>() as u64
                    + if jobs.is_empty() {
                        (paths.capacity() * std::mem::size_of::<VaultRelativePath>()) as u64
                    } else {
                        0
                    };
                let overhead = crate::maintenance_parallel::job_overhead::<
                    Option<crate::vault::StagedFile>,
                >() + (std::mem::size_of::<std::sync::mpsc::Receiver<()>>()
                    + std::mem::size_of::<crate::domain::Result<Option<crate::vault::StagedFile>>>(
                    )) as u64;
                if reserved + reservation + capture_bytes as u64 + overhead
                    > crate::maintenance_parallel::available_bytes()
                {
                    break;
                }
                let (reference, bytes) = pending.next().expect("peeked retained payload");
                eligible &= path_workspace.is_some();
                paths.push(reference.path.clone());
                let work = (fs, prepared, reference, bytes, retained_layout);
                jobs.push(crate::maintenance_parallel::Job::new(
                    reservation,
                    move || {
                        let (fs, prepared, reference, bytes, retained_layout) = work;
                        if crate::storage::layout::active(fs.root())? != retained_layout {
                            return Err(WikiError::invalid(
                                "retained payload route changed during preparation",
                            ));
                        }
                        if retained_layout {
                            if let Some(existing) = crate::storage::layout::raw_read(
                                fs.root(),
                                &reference.path,
                                bytes.len(),
                            )? {
                                if existing != bytes {
                                    return Err(WikiError::new(
                                        ErrorCode::ContentConflict,
                                        "immutable storage bytes changed",
                                    ));
                                }
                                return Ok(None);
                            }
                        }
                        Ok(Some(prepared.stage(&bytes)?))
                    },
                ));
                reserved += reservation + capture_bytes as u64 + overhead;
            }
            if jobs.is_empty() {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "retained staging in-flight allowance exhausted",
                ));
            }
            let batch = if !eligible {
                crate::maintenance_parallel::run_batch_sequential(jobs)?
            } else {
                crate::maintenance_parallel::run_batch(jobs)?
            };
            // Refuse the entire joined chunk before installing any destination if
            // staging failed. Dropping its successes removes all owned temps.
            let mut lease = batch.into_iter();
            // Reverse drop order frees owner descriptor strings before its lease
            // on early reduction/installation errors as well as success.
            let paths = paths;
            let mut stages = Vec::with_capacity(paths.len());
            for result in lease.by_ref() {
                stages.push(result?);
            }
            for (path, staged) in paths.iter().zip(stages) {
                if let Some(staged) = staged {
                    require_sync(self.fs.replace(staged, &ExpectedState::Absent, permit)?)?;
                } else {
                    require_sync(self.fs.sync_target(path, permit)?)?;
                }
            }
            drop(paths);
            drop(lease);
        }
        Ok(())
    }
    fn persist(&self, permit: &WriterPermit, path: &VaultRelativePath, bytes: &[u8]) -> Result<()> {
        let parent = path
            .as_str()
            .rsplit_once('/')
            .expect("retained file has parent")
            .0;
        require_sync(
            self.fs
                .ensure_directory(&VaultRelativePath::new(parent)?, permit)?,
        )?;
        let staged = self.fs.stage(path, bytes, permit)?;
        require_sync(self.fs.replace(staged, &ExpectedState::Absent, permit)?)
    }
}

pub(crate) fn render_prepared_note(
    manifest: &ChangeManifest,
    hash: &Blake3Hash,
) -> Result<Vec<u8>> {
    let json = serde_json::to_vec(manifest).map_err(|e| WikiError::invalid(e.to_string()))?;
    if Blake3Hash::digest(&json) != *hash {
        return Err(WikiError::invalid("manifest renderer identity mismatch"));
    }
    let title =
        serde_json::to_string(&manifest.title).map_err(|e| WikiError::invalid(e.to_string()))?;
    Ok(format!("---\nwiki_schema: \"1\"\nwiki_id: {}\nwiki_kind: change\ntitle: {}\nwiki_status: prepared\nwiki_created_at: {}\nwiki_manifest_hash: {}\n---\n\n```lwiki-change-v1\n{}\n```\n", manifest.change_id, title, manifest.created_at, hash, String::from_utf8(json).expect("JSON UTF8")).into_bytes())
}

pub fn manifest_path(id: &RecordId) -> Result<VaultRelativePath> {
    VaultRelativePath::new(format!("changes/{id}/change.md"))
}
fn payload_path(
    id: &RecordId,
    index: usize,
    side: &str,
    target: &VaultRelativePath,
) -> Result<VaultRelativePath> {
    let path = if target.as_str().ends_with(".md") {
        format!("changes/{id}/{side}/{index:05}.md")
    } else {
        format!("changes/{id}/assets/{side}/{index:05}.bin")
    };
    VaultRelativePath::new(path)
}
fn state(bytes: Option<&[u8]>) -> ExpectedState {
    bytes.map_or(ExpectedState::Absent, |b| {
        ExpectedState::Hash(Blake3Hash::digest(b))
    })
}
fn validate_targets(fs: &VaultFs, targets: &[VaultRelativePath]) -> Result<()> {
    validate_retained_targets(targets)?;
    fs.validate_paths(targets)
}
fn validate_retained_targets(targets: &[VaultRelativePath]) -> Result<()> {
    for path in targets {
        if path.as_str().split('/').any(|part| {
            let folded = unicase::UniCase::unicode(part).to_folded_case();
            matches!(folded.as_str(), ".wiki" | ".git" | "changes")
                || folded.starts_with(".lwiki-stage-")
        }) || path
            .as_str()
            .rsplit('/')
            .next()
            .is_some_and(|part| unicase::UniCase::unicode(part).to_folded_case() == "index.md")
        {
            return Err(WikiError::invalid("reserved changeset target"));
        }
    }
    let mut planned = BTreeMap::<String, String>::new();
    let mut distinct = BTreeSet::new();
    for target in targets {
        if !distinct.insert(target) {
            return Err(WikiError::invalid("duplicate target"));
        }
        let mut prefix = String::new();
        for component in target.as_str().split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(component);
            let folded = unicase::UniCase::unicode(&prefix).to_folded_case();
            if planned
                .insert(folded, prefix.clone())
                .is_some_and(|previous| previous != prefix)
            {
                return Err(WikiError::invalid("case folded target collision"));
            }
        }
    }
    let set: BTreeSet<_> = targets.iter().map(|p| p.as_str()).collect();
    for path in targets {
        for (index, _) in path.as_str().match_indices('/') {
            if set.contains(&path.as_str()[..index]) {
                return Err(WikiError::invalid("overlapping changeset targets"));
            }
        }
    }
    Ok(())
}
fn immutable_kind(bytes: Option<&[u8]>) -> bool {
    bytes.is_some_and(|b| {
        parse_note(b).fields.is_some_and(|fields| {
            fields
                .get("wiki_kind")
                .and_then(Value::as_str)
                .is_some_and(|kind| matches!(kind, "revision" | "extraction_packet" | "run_event"))
        })
    })
}
fn infer_role(
    fs: &VaultFs,
    path: &VaultRelativePath,
    before: Option<&[u8]>,
    after: Option<&[u8]>,
    preparing: bool,
) -> Result<OperationRole> {
    let parts: Vec<_> = path.as_str().split('/').collect();
    let revision = parts.len() >= 5
        && unicase::UniCase::unicode(parts[0]).to_folded_case() == "sources"
        && unicase::UniCase::unicode(parts[2]).to_folded_case() == "revisions";
    let immutable = revision || immutable_kind(before) || immutable_kind(after);
    if immutable && before.is_some() {
        return Err(WikiError::invalid(
            "immutable records/assets cannot be replaced or deleted",
        ));
    }
    if revision && preparing {
        let directory = VaultRelativePath::new(parts[..4].join("/"))?;
        if fs.root().resolve(&directory)?.exists() {
            return Err(WikiError::invalid(
                "cannot add content to existing revision tree",
            ));
        }
    }
    Ok(if immutable {
        OperationRole::ImmutableAsset
    } else {
        OperationRole::MutableRecord
    })
}
fn source_dependencies(
    bytes: Option<&[u8]>,
    targets: &[VaultRelativePath],
) -> Vec<VaultRelativePath> {
    let Some(record) = bytes.and_then(|b| parse_note(b).canonical) else {
        return Vec::new();
    };
    if record.kind() != RecordKind::Source {
        return Vec::new();
    }
    let Some(revision) = record.string("wiki_current_revision") else {
        return Vec::new();
    };
    targets
        .iter()
        .filter(|target| {
            let parts: Vec<_> = target.as_str().split('/').collect();
            parts.len() >= 5
                && unicase::UniCase::unicode(parts[0]).to_folded_case() == "sources"
                && parts[1] == record.id().as_str()
                && unicase::UniCase::unicode(parts[2]).to_folded_case() == "revisions"
                && parts[3] == revision
        })
        .cloned()
        .collect()
}

fn resolve_dependencies(operations: &[ExpectedWrite]) -> Result<Vec<Vec<usize>>> {
    let targets: BTreeMap<_, _> = operations
        .iter()
        .enumerate()
        .map(|(i, op)| (&op.target, i))
        .collect();
    operations
        .iter()
        .map(|op| {
            op.apply_after
                .iter()
                .map(|path| {
                    targets
                        .get(path)
                        .copied()
                        .ok_or_else(|| WikiError::invalid("missing prerequisite target"))
                })
                .collect()
        })
        .collect()
}
pub fn topological_order(dependencies: &[Vec<usize>]) -> Result<Vec<usize>> {
    let mut remaining: Vec<_> = dependencies.iter().map(Vec::len).collect();
    let mut followers = vec![Vec::new(); dependencies.len()];
    let mut ready = BTreeSet::new();
    for (index, deps) in dependencies.iter().enumerate() {
        if deps.iter().any(|d| *d >= dependencies.len() || *d == index)
            || deps.iter().collect::<BTreeSet<_>>().len() != deps.len()
        {
            return Err(WikiError::invalid("invalid operation dependency"));
        }
        for dependency in deps {
            followers[*dependency].push(index);
        }
        if deps.is_empty() {
            ready.insert(index);
        }
    }
    let mut order = Vec::with_capacity(dependencies.len());
    while let Some(next) = ready.pop_first() {
        order.push(next);
        for follower in &followers[next] {
            remaining[*follower] -= 1;
            if remaining[*follower] == 0 {
                ready.insert(*follower);
            }
        }
    }
    if order.len() != dependencies.len() {
        return Err(WikiError::invalid("cyclic operation dependencies"));
    }
    Ok(order)
}
fn io_error(error: std::io::Error) -> WikiError {
    WikiError::new(ErrorCode::Internal, error.to_string())
}
pub(crate) fn read_bounded(
    fs: &VaultFs,
    path: &VaultRelativePath,
    limit: usize,
) -> Result<Option<Vec<u8>>> {
    let observed =
        crate::vault::fs::read_regular_bounded(fs.root(), path, false, limit, "managed-read")
            .map_err(|error| {
                if error.code == ErrorCode::BudgetExceeded {
                    WikiError::invalid(error.message)
                } else {
                    error
                }
            })?;
    match observed {
        Some(bytes) => Ok(Some(bytes)),
        None => crate::storage::layout::legacy_payload(fs, path, limit),
    }
}
pub(super) fn read_with_budget(
    fs: &VaultFs,
    path: &VaultRelativePath,
    remaining: &mut usize,
) -> Result<Option<Vec<u8>>> {
    let resolved = fs.root().resolve(path)?;
    match std::fs::metadata(&resolved) {
        Ok(metadata) if metadata.len() > *remaining as u64 => {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "managed reads exceed aggregate byte ceiling",
            ));
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(io_error(e)),
    }
    let bytes = read_bounded(fs, path, MAX_PAYLOAD_BYTES.min(*remaining))?;
    *remaining = remaining
        .checked_sub(bytes.as_ref().map_or(0, Vec::len))
        .ok_or_else(|| {
            WikiError::new(ErrorCode::BudgetExceeded, "managed read budget exhausted")
        })?;
    Ok(bytes)
}
/// Recursively reject duplicates while streaming, before conversion into any map.
pub(crate) fn strict_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    struct Unique(Value);
    impl<'de> Deserialize<'de> for Unique {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
            struct V;
            impl<'de> Visitor<'de> for V {
                type Value = Unique;
                fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                    f.write_str("bounded JSON without duplicate keys")
                }
                fn visit_bool<E: de::Error>(self, v: bool) -> std::result::Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<Unique, E> {
                    serde_json::Number::from_f64(v)
                        .map(|n| Unique(Value::Number(n)))
                        .ok_or_else(|| E::custom("invalid JSON number"))
                }
                fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_string<E: de::Error>(self, v: String) -> std::result::Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_unit<E: de::Error>(self) -> std::result::Result<Unique, E> {
                    Ok(Unique(Value::Null))
                }
                fn visit_seq<A: SeqAccess<'de>>(
                    self,
                    mut a: A,
                ) -> std::result::Result<Unique, A::Error> {
                    let mut out = Vec::new();
                    while let Some(v) = a.next_element::<Unique>()? {
                        out.push(v.0);
                    }
                    Ok(Unique(Value::Array(out)))
                }
                fn visit_map<A: MapAccess<'de>>(
                    self,
                    mut a: A,
                ) -> std::result::Result<Unique, A::Error> {
                    let mut out = serde_json::Map::new();
                    while let Some(key) = a.next_key::<String>()? {
                        if out.contains_key(&key) {
                            return Err(de::Error::custom("duplicate JSON key"));
                        }
                        out.insert(key, a.next_value::<Unique>()?.0);
                    }
                    Ok(Unique(Value::Object(out)))
                }
            }
            d.deserialize_any(V)
        }
    }
    let unique: Unique =
        serde_json::from_slice(bytes).map_err(|e| WikiError::invalid(e.to_string()))?;
    serde_json::from_value(unique.0).map_err(|e| WikiError::invalid(e.to_string()))
}

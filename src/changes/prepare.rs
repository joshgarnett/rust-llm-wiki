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
    fmt,
    fs::{self, File},
    io::Read,
};

pub const MAX_MANIFEST_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_JOURNAL_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const MAX_OPS: usize = 10_000;
const OPEN: &[u8] = b"```lwiki-change-v1\n";

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
        for operation in operations {
            let before = read_bounded(&self.fs, &operation.target, MAX_PAYLOAD_BYTES)?;
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
        let deps = resolve_dependencies(&plan.operations)?;
        let mut manifest = ChangeManifest {
            version: 1,
            vault_id: self.vault_id.clone(),
            change_id: change_id.clone(),
            title: draft.title,
            created_at,
            origin: draft.origin,
            inverse_of: draft.inverse_of,
            allocated_ids: draft.allocated_ids,
            read_preconditions: plan.read_preconditions.clone(),
            operations: Vec::new(),
        };
        for (index, operation) in plan.operations.iter().enumerate() {
            let before_payload = self.retain(
                permit,
                &change_id,
                index,
                "before",
                plan.before[index].as_deref(),
                &operation.target,
            )?;
            let after_payload = self.retain(
                permit,
                &change_id,
                index,
                "proposed",
                operation.proposed.as_deref(),
                &operation.target,
            )?;
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
        let hash = Blake3Hash::digest(&json);
        let note = render_prepared_note(&manifest, &hash)?;
        self.persist(permit, &manifest_path(&change_id)?, &note)?;
        // Reload validates retained bytes, exact fence identity and manifest before authorization.
        let loaded = self.load_manifest(&change_id)?;
        journal::append_event(
            &self.fs,
            permit,
            &loaded.0,
            &loaded.1,
            ChangeEvent::Prepared,
        )?;
        self.inspect(&change_id)
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
            let change = self.inspect(&id)?;
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
    /// Read-only diagnostics for bytes retained before a manifest became durable.
    /// These directories are preserved, and cannot authorize an application.
    pub fn incomplete_preparations(&self) -> Result<Vec<RecordId>> {
        self.require_binding()?;
        self.classify_change_directories()
            .map(|(_, incomplete)| incomplete)
    }
    fn classify_change_directories(&self) -> Result<(Vec<RecordId>, Vec<RecordId>)> {
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
        Ok((ids, incomplete))
    }
    pub(crate) fn load_manifest(&self, id: &RecordId) -> Result<(ChangeManifest, Blake3Hash)> {
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
        self.validate_manifest(&manifest, id)?;
        Ok((manifest, hash))
    }
    pub(crate) fn validate_manifest(&self, manifest: &ChangeManifest, id: &RecordId) -> Result<()> {
        if manifest.version != 1
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
    pub(crate) fn verify_payload(
        &self,
        id: &RecordId,
        index: usize,
        side: &str,
        target: &VaultRelativePath,
        expected: &ExpectedState,
        payload: &Option<PayloadRef>,
    ) -> Result<Option<Vec<u8>>> {
        match (expected, payload) {
            (ExpectedState::Absent, None) => Ok(None),
            (ExpectedState::Hash(hash), Some(payload)) => {
                if payload.path != payload_path(id, index, side, target)?
                    || &payload.hash != hash
                    || payload.byte_len > MAX_PAYLOAD_BYTES as u64
                {
                    return Err(WikiError::invalid("invalid retained payload reference"));
                }
                let bytes = read_bounded(&self.fs, &payload.path, MAX_PAYLOAD_BYTES)?
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
    fn retain(
        &self,
        permit: &WriterPermit,
        id: &RecordId,
        index: usize,
        side: &str,
        bytes: Option<&[u8]>,
        target: &VaultRelativePath,
    ) -> Result<Option<PayloadRef>> {
        let Some(bytes) = bytes else {
            return Ok(None);
        };
        let path = payload_path(id, index, side, target)?;
        self.persist(permit, &path, bytes)?;
        Ok(Some(PayloadRef {
            path,
            hash: Blake3Hash::digest(bytes),
            byte_len: bytes.len() as u64,
        }))
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
    fs.root().validate_portable_paths(targets)
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
    let path = fs.root().resolve(path)?;
    let mut file = match File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_error(e)),
    };
    let meta = file.metadata().map_err(io_error)?;
    if !meta.is_file() || meta.len() > limit as u64 {
        return Err(WikiError::invalid(
            "managed read is not bounded regular file",
        ));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() > limit {
        return Err(WikiError::invalid("managed read exceeds limit"));
    }
    Ok(Some(bytes))
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

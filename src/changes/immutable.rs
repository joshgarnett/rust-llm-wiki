//! Engine-owned sealed revision membership; graph validation cannot relax this policy.
use super::{
    journal, operation_authority, outcome,
    prepare::{MAX_JOURNAL_BYTES, MAX_PAYLOAD_BYTES, read_bounded, strict_json},
    types::*,
};
use crate::{
    domain::{Blake3Hash, ErrorCode, RecordId, Result, VaultRelativePath, WikiError},
    vault::{ExpectedState, WriterPermit},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Tree {
    root: VaultRelativePath,
    members: BTreeMap<VaultRelativePath, Blake3Hash>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ownership {
    version: u32,
    change: PreparedChange,
    trees: Vec<Tree>,
    validation_hash: Blake3Hash,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    ownership: Ownership,
    checksum: Blake3Hash,
}

/// Namespace words are case-insensitive; source/revision IDs remain case-sensitive.
fn tree_path(target: &VaultRelativePath) -> Result<Option<VaultRelativePath>> {
    let parts: Vec<_> = target.as_str().split('/').collect();
    if parts.len() >= 5
        && unicase::UniCase::unicode(parts[0]).to_folded_case() == "sources"
        && unicase::UniCase::unicode(parts[2]).to_folded_case() == "revisions"
    {
        Ok(Some(VaultRelativePath::new(parts[..4].join("/"))?))
    } else {
        Ok(None)
    }
}
fn key(tree: &VaultRelativePath) -> (&str, &str) {
    let mut parts = tree.as_str().split('/');
    parts.next();
    let source = parts.next().expect("source");
    parts.next();
    (source, parts.next().expect("revision"))
}
fn receipt_path(id: &RecordId) -> Result<VaultRelativePath> {
    VaultRelativePath::new(format!("changes/{id}/revision-trees.json"))
}
fn tree_inventory(manifest: &ChangeManifest) -> Result<Vec<Tree>> {
    let mut trees = BTreeMap::<VaultRelativePath, BTreeMap<VaultRelativePath, Blake3Hash>>::new();
    for operation in &manifest.operations {
        if let Some(root) = tree_path(&operation.target)? {
            if operation.role != OperationRole::ImmutableAsset
                || operation.before != ExpectedState::Absent
            {
                return Err(WikiError::invalid(
                    "revision assets require immutable create operations",
                ));
            }
            let ExpectedState::Hash(after) = &operation.after else {
                return Err(WikiError::invalid("revision assets cannot be deleted"));
            };
            trees
                .entry(root)
                .or_default()
                .insert(operation.target.clone(), after.clone());
        }
    }
    Ok(trees
        .into_iter()
        .map(|(root, members)| Tree { root, members })
        .collect())
}
fn planned(
    engine: &ChangeEngine,
    manifest: &ChangeManifest,
    hash: &Blake3Hash,
) -> Result<Ownership> {
    let baseline_path =
        VaultRelativePath::new(format!("changes/{}/validation.json", manifest.change_id))?;
    let baseline =
        read_bounded(&engine.fs, &baseline_path, MAX_JOURNAL_BYTES)?.ok_or_else(|| {
            super::apply::recovery_error(
                "revision ownership requires its original retained validation baseline",
            )
        })?;
    Ok(Ownership {
        version: 1,
        change: PreparedChange {
            change_id: manifest.change_id.clone(),
            manifest_hash: hash.clone(),
        },
        trees: tree_inventory(manifest)?,
        validation_hash: Blake3Hash::digest(baseline),
    })
}
fn load(engine: &ChangeEngine, expected: &Ownership) -> Result<Option<Ownership>> {
    let Some(bytes) = read_bounded(
        &engine.fs,
        &receipt_path(&expected.change.change_id)?,
        MAX_JOURNAL_BYTES,
    )?
    else {
        return Ok(None);
    };
    let receipt: Receipt = strict_json(&bytes)?;
    let checksum = Blake3Hash::digest(
        serde_json::to_vec(&receipt.ownership).map_err(|e| WikiError::invalid(e.to_string()))?,
    );
    if checksum != receipt.checksum || &receipt.ownership != expected {
        return Err(WikiError::invalid(
            "revision ownership receipt does not match verified manifest",
        ));
    }
    Ok(Some(receipt.ownership))
}
fn conflict(message: impl Into<String>) -> WikiError {
    WikiError::new(ErrorCode::ContentConflict, message)
}

impl ChangeEngine {
    /// Abandonment cannot release ownership of a tree with unplanned members.
    /// Conservatively require the complete original revision namespace absent.
    pub(crate) fn require_abandoned_revision_trees(&self, manifest: &ChangeManifest) -> Result<()> {
        for tree in tree_inventory(manifest)? {
            match fs::symlink_metadata(self.fs.root().resolve(&tree.root)?) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(WikiError::new(ErrorCode::Internal, error.to_string())),
                Ok(_) => {
                    return Err(conflict(format!(
                        "abandon requires revision tree {} absent; preserve unfamiliar members, remove only verified empty directories, or resolve by resuming",
                        tree.root
                    )));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn require_revision_baseline(&self, manifest: &ChangeManifest) -> Result<()> {
        if read_bounded(
            &self.fs,
            &receipt_path(&manifest.change_id)?,
            MAX_JOURNAL_BYTES,
        )?
        .is_some()
        {
            let baseline =
                VaultRelativePath::new(format!("changes/{}/validation.json", manifest.change_id))?;
            if read_bounded(&self.fs, &baseline, MAX_JOURNAL_BYTES)?.is_none() {
                return Err(super::apply::recovery_error(
                    "retained revision ownership lost its original validation baseline",
                ));
            }
        }
        Ok(())
    }
    /// Establish intent-independent tree ownership evidence while all new trees are absent.
    /// A known Prepared journal still cannot assume that matching existing bytes are its writes.
    pub(crate) fn preflight_revision_trees(
        &self,
        permit: &WriterPermit,
        manifest: &ChangeManifest,
        hash: &Blake3Hash,
        state: &JournalState,
    ) -> Result<()> {
        self.preflight_revision_trees_with(permit, manifest, hash, state, &|ownership| {
            self.check_competing_owners(ownership)
        })
    }
    fn preflight_revision_trees_with(
        &self,
        permit: &WriterPermit,
        manifest: &ChangeManifest,
        hash: &Blake3Hash,
        state: &JournalState,
        owners: &dyn Fn(&Ownership) -> Result<()>,
    ) -> Result<()> {
        permit.require_root(self.fs.root())?;
        if tree_inventory(manifest)?.is_empty() {
            return Ok(());
        }
        let ownership = planned(self, manifest, hash)?;
        owners(&ownership)?;
        let retained = load(self, &ownership)?;
        let applying = matches!(
            state.status,
            Some(ChangeStatus::Applying | ChangeStatus::FilesApplied | ChangeStatus::Indexed)
        );
        if applying && retained.is_none() {
            return Err(super::apply::recovery_error(
                "applying revision change lost its retained ownership proof",
            ));
        }
        for tree in &ownership.trees {
            let path = self.fs.root().resolve(&tree.root)?;
            let exists = match fs::symlink_metadata(&path) {
                Ok(_) => true,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
                Err(e) => return Err(WikiError::new(ErrorCode::Internal, e.to_string())),
            };
            if exists && (state.status == Some(ChangeStatus::Prepared) || retained.is_none()) {
                return Err(conflict(format!(
                    "revision tree {} was independently created after preparation",
                    tree.root
                )));
            }
            check_members(self, tree, manifest, false)?;
        }
        if retained.is_some() {
            return journal::require_sync(
                self.fs
                    .sync_target(&receipt_path(&manifest.change_id)?, permit)?,
            );
        }
        let checksum = Blake3Hash::digest(
            serde_json::to_vec(&ownership).map_err(|e| WikiError::invalid(e.to_string()))?,
        );
        let bytes = serde_json::to_vec(&Receipt {
            ownership,
            checksum,
        })
        .map_err(|e| WikiError::invalid(e.to_string()))?;
        if bytes.len() > MAX_JOURNAL_BYTES {
            return Err(WikiError::invalid(
                "revision ownership receipt exceeds limit",
            ));
        }
        let staged = self
            .fs
            .stage(&receipt_path(&manifest.change_id)?, &bytes, permit)?;
        journal::require_sync(self.fs.replace(staged, &ExpectedState::Absent, permit)?)
    }
    /// Revalidate every tree's complete membership around mutations and before publication.
    pub(crate) fn verify_revision_trees(
        &self,
        permit: &WriterPermit,
        manifest: &ChangeManifest,
        hash: &Blake3Hash,
        complete: bool,
    ) -> Result<()> {
        self.verify_revision_trees_with(permit, manifest, hash, complete, &|ownership| {
            self.check_competing_owners(ownership)
        })
    }
    fn verify_revision_trees_with(
        &self,
        permit: &WriterPermit,
        manifest: &ChangeManifest,
        hash: &Blake3Hash,
        complete: bool,
        owners: &dyn Fn(&Ownership) -> Result<()>,
    ) -> Result<()> {
        permit.require_root(self.fs.root())?;
        if tree_inventory(manifest)?.is_empty() {
            return Ok(());
        }
        let ownership = planned(self, manifest, hash)?;
        if load(self, &ownership)?.is_none() {
            return Err(super::apply::recovery_error(
                "missing verified revision ownership receipt",
            ));
        }
        owners(&ownership)?;
        for tree in &ownership.trees {
            check_members(self, tree, manifest, complete)?;
        }
        Ok(())
    }
    fn check_competing_owners(&self, current: &Ownership) -> Result<()> {
        for id in self.change_ids()? {
            if id == current.change.change_id {
                continue;
            }
            let (manifest, hash) = self.load_manifest_structure(&id)?;
            let other_trees = tree_inventory(&manifest)?;
            if !other_trees
                .iter()
                .any(|t| current.trees.iter().any(|c| key(&c.root) == key(&t.root)))
            {
                continue;
            }
            let terminal = outcome::terminal_report(&self.fs, &manifest, &hash)?;
            if terminal
                .as_ref()
                .is_some_and(|r| r.status == ChangeStatus::Aborted)
            {
                continue;
            }
            if terminal.is_none() {
                self.validate_manifest(&manifest, &id)?;
            }
            let state = journal::load_journal(&self.fs, &manifest, &hash)?;
            let active = terminal
                .as_ref()
                .is_some_and(|r| r.status == ChangeStatus::Committed)
                || matches!(
                    state.status,
                    Some(
                        ChangeStatus::Applying
                            | ChangeStatus::FilesApplied
                            | ChangeStatus::Indexed
                            | ChangeStatus::Committed
                            | ChangeStatus::Conflict
                    )
                )
                || state.status.is_none()
                    && read_bounded(&self.fs, &receipt_path(&id)?, MAX_JOURNAL_BYTES)?.is_some();
            if active
                && terminal
                    .as_ref()
                    .is_none_or(|r| r.status != ChangeStatus::Committed)
                && state.status.is_none()
            {
                let other = planned(self, &manifest, &hash)?;
                load(self, &other)?.ok_or_else(|| {
                    super::apply::recovery_error("competing owner lost its proof")
                })?;
            }
            if active {
                return Err(conflict(format!(
                    "revision tree is already owned by retained change {id}"
                )));
            }
        }
        Ok(())
    }
}
fn indexed_key(root: &VaultRelativePath) -> RevisionTreeKey {
    let (source, revision) = key(root);
    RevisionTreeKey {
        source_component: source.to_owned(),
        revision_component: revision.to_owned(),
    }
}
fn owner_row(tree: &Tree, change: &PreparedChange) -> RevisionOwnerRow {
    RevisionOwnerRow {
        key: indexed_key(&tree.root),
        change: change.clone(),
    }
}

/// A bounded guard can only be created for the exact retained manifest named by
/// active authority, using the exact starting published epoch. It never replaces
/// the ownership receipt or member checks, only competing-owner discovery.
pub(crate) struct IndexedRevisionGuard<'a> {
    engine: &'a ChangeEngine,
    writer: &'a WriterPermit,
    change: &'a PreparedChange,
    lookup: &'a dyn RevisionOwnershipLookup,
    manifest: ChangeManifest,
    authority: operation_authority::Authority,
    snapshot: crate::domain::ReadSnapshot,
}
impl ChangeEngine {
    pub(crate) fn indexed_revision_guard<'a>(
        &'a self,
        writer: &'a WriterPermit,
        change: &'a PreparedChange,
        lookup: &'a dyn RevisionOwnershipLookup,
    ) -> Result<IndexedRevisionGuard<'a>> {
        writer.require_root(self.fs.root())?;
        self.require_binding()?;
        lookup.require_ready()?;
        if lookup.vault_id() != &self.vault_id {
            return Err(super::apply::recovery_error(
                "revision ownership lookup belongs to another vault",
            ));
        }
        let authority = operation_authority::load(
            &self.fs,
            &self.vault_id,
            operation_authority::Presence::Required,
        )?
        .ok_or_else(|| {
            super::apply::recovery_error("revision guard requires operational authority")
        })?;
        let active = authority
            .active()
            .filter(|active| &active.change == change)
            .ok_or_else(|| {
                super::apply::recovery_error("revision guard requires the exact active change")
            })?;
        let snapshot = lookup.snapshot();
        if snapshot.generation != active.starting.epoch
            || !snapshot.publication().is_some_and(|published| {
                published.version == 1 && published.file_id == active.starting.file_id
            })
        {
            return Err(super::apply::recovery_error(
                "revision ownership lookup differs from active starting publication",
            ));
        }
        let (manifest, hash) = self.load_manifest_structure(&change.change_id)?;
        if hash != change.manifest_hash {
            return Err(super::apply::recovery_error(
                "revision guard retained manifest binding changed",
            ));
        }
        Ok(IndexedRevisionGuard {
            engine: self,
            writer,
            change,
            lookup,
            manifest,
            authority,
            snapshot: snapshot.clone(),
        })
    }

    /// Explicit rebuild/migration only. Existing classification collects change
    /// IDs once; manifests and terminal receipts are processed one at a time.
    /// The sink must reject conflicting owners for a key before marking its
    /// candidate ready. Never infer these historical roots from current files.
    pub(crate) fn reconstruct_revision_owners(
        &self,
        writer: &WriterPermit,
        current: Option<&PreparedChange>,
        progress: &mut dyn FnMut() -> Result<()>,
        emit: &mut dyn FnMut(RevisionOwnerRow) -> Result<()>,
    ) -> Result<()> {
        progress()?;
        writer.require_root(self.fs.root())?;
        self.require_binding()?;
        if let Some(authority) = operation_authority::load(
            &self.fs,
            &self.vault_id,
            operation_authority::Presence::LegacyMayBeAbsent,
        )? && let Some(active) = authority.active()
            && current != Some(&active.change)
        {
            return Err(super::apply::recovery_error(
                "ownership reconstruction cannot bypass another active operation",
            ));
        }
        let mut found_current = current.is_none();
        for id in self.change_ids_checked(progress)? {
            progress()?;
            let (manifest, hash) = self.load_manifest_structure(&id)?;
            let change = PreparedChange {
                change_id: id,
                manifest_hash: hash,
            };
            let is_current = current.is_some_and(|expected| expected.change_id == change.change_id);
            if is_current && current != Some(&change) {
                return Err(super::apply::recovery_error(
                    "ownership reconstruction current manifest changed",
                ));
            }
            if let Some(terminal) =
                outcome::terminal_report(&self.fs, &manifest, &change.manifest_hash)?
            {
                progress()?;
                // Valid historical outcomes deliberately do not require obsolete
                // canonical trees, retained payloads, or validation baselines.
                if terminal.status == ChangeStatus::Committed {
                    for tree in tree_inventory(&manifest)? {
                        emit(owner_row(&tree, &change))?;
                    }
                }
                if is_current {
                    return Err(super::apply::recovery_error(
                        "current ownership reconstruction requires an in-flight legacy publication",
                    ));
                }
                continue;
            }
            let state = journal::load_journal(&self.fs, &manifest, &change.manifest_hash)?;
            progress()?;
            if is_current {
                if !matches!(
                    state.status,
                    Some(ChangeStatus::FilesApplied | ChangeStatus::Indexed)
                ) {
                    return Err(super::apply::recovery_error(
                        "current ownership reconstruction lacks files-applied intent",
                    ));
                }
                let ownership = planned(self, &manifest, &change.manifest_hash)?;
                if !ownership.trees.is_empty() && load(self, &ownership)?.is_none() {
                    return Err(super::apply::recovery_error(
                        "current ownership reconstruction requires its original ownership receipt",
                    ));
                }
                for tree in &ownership.trees {
                    check_members(self, tree, &manifest, true)?;
                    emit(owner_row(tree, &change))?;
                }
                found_current = true;
                continue;
            }
            match state.status {
                Some(ChangeStatus::Prepared) => {}
                None => {
                    if read_bounded(
                        &self.fs,
                        &receipt_path(&change.change_id)?,
                        MAX_JOURNAL_BYTES,
                    )?
                    .is_some()
                        || self
                            .observe(&manifest)?
                            .iter()
                            .any(|observed| observed.observed != observed.before)
                    {
                        return Err(super::apply::recovery_error(
                            "unresolved staged ownership blocks reconstruction",
                        ));
                    }
                }
                _ => {
                    return Err(super::apply::recovery_error(
                        "unresolved or unretained terminal change blocks ownership reconstruction",
                    ));
                }
            }
        }
        if !found_current {
            return Err(super::apply::recovery_error(
                "current ownership reconstruction manifest is missing",
            ));
        }
        progress()?;
        Ok(())
    }
}
impl IndexedRevisionGuard<'_> {
    fn require_active(&self) -> Result<()> {
        self.writer.require_root(self.engine.fs.root())?;
        self.engine.require_binding()?;
        let authority = operation_authority::load(
            &self.engine.fs,
            &self.engine.vault_id,
            operation_authority::Presence::Required,
        )?
        .ok_or_else(|| {
            super::apply::recovery_error("revision guard operation authority disappeared")
        })?;
        if !self.authority.same_revision(&authority)
            || self.lookup.snapshot() != &self.snapshot
            || self.lookup.vault_id() != &self.engine.vault_id
        {
            return Err(super::apply::recovery_error(
                "revision guard active operation or pinned publication changed",
            ));
        }
        // The retained manifest is immutable authority, not caller-owned input.
        let (manifest, hash) = self
            .engine
            .load_manifest_structure(&self.change.change_id)?;
        if hash != self.change.manifest_hash || manifest != self.manifest {
            return Err(super::apply::recovery_error(
                "revision guard retained manifest changed",
            ));
        }
        Ok(())
    }
    fn check_owners(&self, current: &Ownership) -> Result<()> {
        for tree in &current.trees {
            let requested = indexed_key(&tree.root);
            let Some(owner) = self.lookup.revision_owner(&requested)? else {
                continue;
            };
            if owner == *self.change {
                continue;
            }
            let (manifest, hash) = self.engine.load_manifest_structure(&owner.change_id)?;
            if hash != owner.manifest_hash
                || !tree_inventory(&manifest)?
                    .iter()
                    .any(|other| key(&other.root) == key(&tree.root))
            {
                return Err(WikiError::new(
                    ErrorCode::IndexCorrupt,
                    "indexed revision owner differs from retained manifest",
                ));
            }
            if !outcome::terminal_report(&self.engine.fs, &manifest, &hash)?
                .is_some_and(|terminal| terminal.status == ChangeStatus::Committed)
            {
                return Err(super::apply::recovery_error(
                    "indexed revision owner lacks its committed terminal receipt",
                ));
            }
            return Err(conflict(format!(
                "revision tree is already owned by retained change {}",
                owner.change_id
            )));
        }
        Ok(())
    }
    pub(crate) fn preflight(&self) -> Result<()> {
        self.require_active()?;
        let state =
            journal::load_journal(&self.engine.fs, &self.manifest, &self.change.manifest_hash)?;
        if !matches!(
            state.status,
            None | Some(
                ChangeStatus::Prepared
                    | ChangeStatus::Applying
                    | ChangeStatus::FilesApplied
                    | ChangeStatus::Indexed
            )
        ) {
            return Err(super::apply::recovery_error(
                "revision guard requires an applicable active journal",
            ));
        }
        self.engine.preflight_revision_trees_with(
            self.writer,
            &self.manifest,
            &self.change.manifest_hash,
            &state,
            &|ownership| self.check_owners(ownership),
        )
    }
    pub(crate) fn verify(&self, complete: bool) -> Result<()> {
        self.require_active()?;
        self.engine.verify_revision_trees_with(
            self.writer,
            &self.manifest,
            &self.change.manifest_hash,
            complete,
            &|ownership| self.check_owners(ownership),
        )
    }
    pub(crate) fn complete_owner_rows(&self) -> Result<Vec<RevisionOwnerRow>> {
        self.verify(true)?;
        let state =
            journal::load_journal(&self.engine.fs, &self.manifest, &self.change.manifest_hash)?;
        if !matches!(
            state.status,
            Some(ChangeStatus::FilesApplied | ChangeStatus::Indexed)
        ) {
            return Err(super::apply::recovery_error(
                "completed owner rows require files-applied intent",
            ));
        }
        Ok(tree_inventory(&self.manifest)?
            .iter()
            .map(|tree| owner_row(tree, self.change))
            .collect())
    }
}

fn check_members(
    engine: &ChangeEngine,
    tree: &Tree,
    manifest: &ChangeManifest,
    complete: bool,
) -> Result<()> {
    let root = engine.fs.root().resolve(&tree.root)?;
    match fs::symlink_metadata(&root) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if complete {
                return Err(conflict(format!(
                    "revision tree {} is incomplete",
                    tree.root
                )));
            }
            return Ok(());
        }
        Err(e) => return Err(WikiError::new(ErrorCode::Internal, e.to_string())),
        Ok(meta) if !meta.is_dir() || meta.file_type().is_symlink() => {
            return Err(conflict(format!(
                "revision tree {} is not a regular directory",
                tree.root
            )));
        }
        Ok(_) => {}
    }
    let mut directories = BTreeSet::new();
    directories.insert(tree.root.clone());
    for member in tree.members.keys() {
        let mut ancestor = member.as_str();
        while let Some((parent, _)) = ancestor.rsplit_once('/') {
            if parent.len() < tree.root.as_str().len() {
                break;
            }
            directories.insert(VaultRelativePath::new(parent)?);
            ancestor = parent;
        }
    }
    let mut found = BTreeSet::new();
    walk(engine, tree, manifest, &tree.root, &directories, &mut found)?;
    if complete && found.len() != tree.members.len() {
        return Err(conflict(format!(
            "revision tree {} has missing planned members",
            tree.root
        )));
    }
    Ok(())
}
fn walk(
    engine: &ChangeEngine,
    tree: &Tree,
    manifest: &ChangeManifest,
    directory: &VaultRelativePath,
    allowed: &BTreeSet<VaultRelativePath>,
    found: &mut BTreeSet<VaultRelativePath>,
) -> Result<()> {
    let absolute = engine.fs.root().resolve(directory)?;
    for entry in
        fs::read_dir(absolute).map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))?
    {
        let entry = entry.map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| conflict("non-UTF8 member in revision tree"))?;
        let path = VaultRelativePath::new(format!("{directory}/{name}"))?;
        let metadata = entry
            .file_type()
            .map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))?;
        if metadata.is_symlink() {
            return Err(conflict(format!(
                "revision tree member {path} is a symlink"
            )));
        }
        if metadata.is_dir() {
            if !allowed.contains(&path) {
                return Err(conflict(format!(
                    "unplanned directory {path} in sealed revision tree"
                )));
            }
            walk(engine, tree, manifest, &path, allowed, found)?;
        } else if metadata.is_file() {
            if let Some(expected) = tree.members.get(&path) {
                let bytes = read_bounded(&engine.fs, &path, MAX_PAYLOAD_BYTES)?
                    .ok_or_else(|| conflict("revision member disappeared"))?;
                if Blake3Hash::digest(bytes) != *expected {
                    return Err(conflict(format!(
                        "revision tree member {path} has unfamiliar bytes"
                    )));
                }
                found.insert(path);
            } else if !private_stage(engine, tree, manifest, &path, &name)? {
                return Err(conflict(format!(
                    "unplanned member {path} in sealed revision tree"
                )));
            }
        } else {
            return Err(conflict(format!(
                "unsupported member type {path} in revision tree"
            )));
        }
    }
    Ok(())
}
fn private_stage(
    engine: &ChangeEngine,
    tree: &Tree,
    manifest: &ChangeManifest,
    path: &VaultRelativePath,
    name: &str,
) -> Result<bool> {
    let Some(id) = name
        .strip_prefix(".lwiki-stage-")
        .and_then(|n| n.strip_suffix(".tmp"))
    else {
        return Ok(false);
    };
    if !uuid::Uuid::parse_str(id).is_ok_and(|id| id.get_version_num() == 7) {
        return Ok(false);
    }
    let parent = path.as_str().rsplit_once('/').expect("stage parent").0;
    let bytes = read_bounded(&engine.fs, path, MAX_PAYLOAD_BYTES)?
        .ok_or_else(|| conflict("stage disappeared"))?;
    for (index, operation) in manifest.operations.iter().enumerate() {
        if tree.members.contains_key(&operation.target)
            && operation
                .target
                .as_str()
                .rsplit_once('/')
                .is_some_and(|(p, _)| p == parent)
            && let Some(proposed) = engine.verify_payload(
                &manifest.change_id,
                index,
                "proposed",
                &operation.target,
                &operation.after,
                &operation.after_payload,
            )?
            && proposed.starts_with(&bytes)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
#[path = "immutable_indexed_tests.rs"]
mod indexed_tests;

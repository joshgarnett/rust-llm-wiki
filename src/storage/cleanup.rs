use super::{
    inventory::{inventory, validate},
    layout,
    types::*,
};
use crate::{
    catalog::{Catalog, CatalogGraphValidator},
    changes::{
        ChangeEngine, ChangeInspection, ChangeManifest, ChangeStatus, PayloadRef, PreparedChange,
    },
    domain::*,
    jobs::checkpoint,
    vault::{ExpectedState, VaultFs, VaultRoot, WriterPermit, operational::RunStore},
};
use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
    time::Duration,
};
const PENDING: &str = ".wiki/state/storage/cleanup.json";
const MAX_RECEIPT: usize = 64 * 1024 * 1024;

struct ImportEpochMemo {
    root: VaultRoot,
    owner: Rc<()>,
    suspended: usize,
    validated: Option<layout::Layout>,
}
std::thread_local! {
    static IMPORT_EPOCH: RefCell<Option<ImportEpochMemo>> = const { RefCell::new(None) };
}

/// One import's successful semantic predicate, never a filesystem observation.
/// The Rc binding keeps teardown on the originating thread and prevents a
/// stale nested guard from modifying a later operation's scope.
pub(crate) struct ImportEpochScope {
    binding: Option<Rc<()>>,
    owner: bool,
}
impl ImportEpochScope {
    pub(crate) fn begin(root: &VaultRoot, enabled: bool) -> Self {
        IMPORT_EPOCH.with(|state| {
            let mut state = state.borrow_mut();
            if let Some(memo) = state.as_mut() {
                memo.validated = None;
                memo.suspended += 1;
                return Self {
                    binding: Some(memo.owner.clone()),
                    owner: false,
                };
            }
            let binding = enabled.then(|| Rc::new(()));
            if let Some(owner) = &binding {
                *state = Some(ImportEpochMemo {
                    root: root.clone(),
                    owner: owner.clone(),
                    suspended: 0,
                    validated: None,
                });
            }
            Self {
                binding,
                owner: true,
            }
        })
    }
}
impl Drop for ImportEpochScope {
    fn drop(&mut self) {
        IMPORT_EPOCH.with(|state| {
            let mut state = state.borrow_mut();
            let Some(memo) = state.as_mut() else {
                return;
            };
            if !self
                .binding
                .as_ref()
                .is_some_and(|owner| Rc::ptr_eq(owner, &memo.owner))
            {
                return;
            }
            if self.owner {
                *state = None;
            } else {
                memo.suspended -= 1;
            }
        });
    }
}
fn memoized_epoch(root: Option<&VaultRoot>, activation: &layout::Layout, remember: bool) -> bool {
    IMPORT_EPOCH.with(|state| {
        let mut state = state.borrow_mut();
        let Some(memo) = state.as_mut() else {
            return false;
        };
        if root != Some(&memo.root) || memo.suspended != 0 {
            return false;
        }
        if remember {
            memo.validated = Some(activation.clone());
        }
        let hit = memo.validated.as_ref() == Some(activation);
        #[cfg(test)]
        if hit && !remember {
            import_epoch_memo_tests::reused_epoch();
        }
        hit
    })
}
pub(crate) fn invalidate_import_epoch(root: &VaultRoot) {
    IMPORT_EPOCH.with(|state| {
        if let Some(memo) = state.borrow_mut().as_mut()
            && &memo.root == root
        {
            memo.validated = None;
        }
    });
}
fn err(message: &str) -> WikiError {
    WikiError::new(ErrorCode::RecoveryRequired, message)
}
fn rel(value: impl Into<String>) -> Result<VaultRelativePath> {
    VaultRelativePath::new(value)
}
fn existing_runs(fs: &VaultFs, vault: &RecordId) -> Result<Vec<RecordId>> {
    RunStore::discover_existing(fs, vault, 4096, &mut || Ok(()))
}
struct Analysis {
    inventory: StorageInventory,
    changes: Vec<ChangeInspection>,
    protected_changes: BTreeSet<RecordId>,
    histories: Vec<checkpoint::StorageRunHistory>,
    expired: BTreeSet<ExpiryKey>,
    public: StoragePlan,
}
fn permanent_reason(m: &ChangeManifest) -> Option<&'static str> {
    if m.origin.is_some() || m.inverse_of.is_some() {
        Some("import_replay_or_graph_ancestry")
    } else if m
        .operations
        .iter()
        .any(|op| op.target.as_str().starts_with("sources/"))
    {
        Some("source_revision_provenance")
    } else if m.operations.iter().any(|op| {
        op.target.as_str().starts_with("knowledge/")
            || op.target.as_str().contains("/outputs/")
            || op.target.as_str().ends_with("/research.md")
    }) {
        Some("knowledge_or_handoff_provenance")
    } else {
        None
    }
}
fn analyze(fs: &VaultFs, options: &StorageOptions) -> Result<Analysis> {
    validate(options)?;
    let inventory = inventory(fs, options)?;
    let engine = ChangeEngine::new(fs.clone())?;
    let mut changes = Vec::new();
    let mut protected = Vec::new();
    let mut blockers = Vec::new();
    if !inventory.complete {
        blockers.push(
            "inventory incomplete; raise bounds or reconcile protected filesystem entries".into(),
        );
    }
    for id in engine.change_ids()? {
        changes.push(engine.inspect_history(&id)?);
    }
    changes.sort_by(|a, b| {
        (&a.manifest.created_at, &a.manifest.change_id)
            .cmp(&(&b.manifest.created_at, &b.manifest.change_id))
    });
    let expired = completed_expiries(fs, &inventory, &changes)?;
    let recent: BTreeSet<_> = changes
        .iter()
        .rev()
        .filter(|change| {
            matches!(
                change.status,
                ChangeStatus::Committed | ChangeStatus::Aborted
            )
        })
        .take(options.retain_undo_changes)
        .map(|change| change.manifest.change_id.clone())
        .collect();
    let mut protected_changes = BTreeSet::new();
    for change in &changes {
        let m = &change.manifest;
        let reason = if !matches!(
            change.status,
            ChangeStatus::Committed | ChangeStatus::Aborted
        ) {
            Some("unresolved_or_staged_change")
        } else if recent.contains(&m.change_id) {
            if expired.iter().any(|key| key.0 == m.change_id) {
                Some("undo_window_previously_expired_payloads_remain_unavailable")
            } else {
                Some("retained_undo_window")
            }
        } else {
            permanent_reason(m)
        };
        if let Some(reason) = reason {
            protected_changes.insert(m.change_id.clone());
            protected.push(ProtectedStorage {
                path: rel(format!("changes/{}", m.change_id))?,
                reason: reason.into(),
            });
        }
        if inventory.layout_version == 1
            && (m
                .read_preconditions
                .iter()
                .any(|read| read.path.as_str() == "WIKI.md")
                && !matches!(
                    change.status,
                    ChangeStatus::Committed | ChangeStatus::Aborted
                )
                || !matches!(
                    change.status,
                    ChangeStatus::Committed | ChangeStatus::Aborted | ChangeStatus::Prepared
                )
                || change.status == ChangeStatus::Prepared
                    && crate::changes::prepare::read_bounded(
                        fs,
                        &rel(format!("changes/{}/validation.json", m.change_id))?,
                        crate::changes::prepare::MAX_JOURNAL_BYTES,
                    )?
                    .is_some())
        {
            blockers.push(format!(
                "resolve/apply/abandon change {} before upgrading the vault marker",
                m.change_id
            ));
        }
    }
    let mut histories = Vec::new();
    for run in existing_runs(fs, &inventory.vault_id)? {
        let history = checkpoint::storage_run_history(fs, &run)?;
        if inventory.layout_version == 1 && history.binds_vault_marker {
            blockers.push(format!(
                "active job {run} binds WIKI.md; complete or reconcile it before format upgrade"
            ));
        }
        protected.push(ProtectedStorage {
            path: rel(format!(".wiki/state/jobs/{run}"))?,
            reason: "complete_accounting_history_and_unknown_charge_holds".into(),
        });
        histories.push(history);
    }
    let known_payloads: BTreeMap<_, _> = changes
        .iter()
        .flat_map(|change| {
            change.manifest.operations.iter().flat_map(move |op| {
                [&op.before_payload, &op.after_payload]
                    .into_iter()
                    .flatten()
                    .map(move |r| {
                        (
                            r.path.clone(),
                            (change.manifest.change_id.clone(), r.clone()),
                        )
                    })
            })
        })
        .collect();
    let mirrors: BTreeMap<_, _> = histories
        .iter()
        .flat_map(|h| {
            h.mirrors
                .iter()
                .map(|(path, hash, len)| (path.clone(), (hash.clone(), *len)))
        })
        .collect();
    let mut known_objects = BTreeSet::new();
    let mut live_objects = BTreeSet::new();
    for change in &changes {
        for reference in change
            .manifest
            .operations
            .iter()
            .flat_map(|op| [&op.before_payload, &op.after_payload])
            .flatten()
        {
            let path = layout::object_path(&reference.hash)?;
            known_objects.insert(path.clone());
            if protected_changes.contains(&change.manifest.change_id) {
                live_objects.insert(path);
            }
        }
    }
    let mut candidates = Vec::new();
    for file in &inventory.files {
        let logical = layout::logical_path(&file.path).unwrap_or_else(|| file.path.clone());
        let candidate = (known_objects.contains(&file.path) && !live_objects.contains(&file.path))
            || layout::managed_path(&file.path).is_some()
            || known_payloads
                .get(&logical)
                .is_some_and(|(change, _)| !protected_changes.contains(change))
            || mirrors
                .get(&logical)
                .is_some_and(|(hash, len)| hash == &file.hash && *len == file.bytes);
        if candidate {
            candidates.push(file.clone());
        } else {
            protected.push(ProtectedStorage {
                path: file.path.clone(),
                reason: if file.path.as_str().starts_with("sources/") {
                    "canonical_source_evidence"
                } else {
                    "canonical_active_accounting_or_unclassified"
                }
                .into(),
            });
        }
    }
    let hash = Blake3Hash::digest(
        serde_json::to_vec(&(
            inventory.vault_id.clone(),
            inventory.layout_version,
            options.retain_undo_changes,
            inventory
                .files
                .iter()
                .filter(|file| {
                    let path = file.path.as_str();
                    path != ".wiki/state/writer.lock"
                        && !path.starts_with(".wiki/cache/")
                        && !(path.starts_with(".wiki/state/jobs/")
                            && path.ends_with("/ledger.lock"))
                })
                .collect::<Vec<_>>(),
            options.max_files,
            options.max_bytes,
            &blockers,
        ))
        .map_err(|e| WikiError::invalid(e.to_string()))?,
    );
    let public = StoragePlan {
        vault_id: inventory.vault_id.clone(),
        plan_hash: hash,
        layout_version: inventory.layout_version,
        retain_undo_changes: options.retain_undo_changes,
        before: inventory.totals.clone(),
        candidates,
        protected,
        blockers,
        full_backup_required: true,
        planned_copy_bytes: 0, planned_delete_bytes: 0, estimated_net_bytes: 0,
        estimate_excludes: "New compact proofs, payload maps, epoch/receipt metadata and unrelated cache changes; positive estimated_net_bytes means fewer retained bytes. Actual totals are measured after cleanup.".into(),
    };
    Ok(Analysis {
        inventory,
        changes,
        protected_changes,
        histories,
        expired,
        public,
    })
}
pub fn plan_cleanup(fs: &VaultFs, options: &StorageOptions) -> Result<StoragePlan> {
    let analysis = analyze(fs, options)?;
    let mut plan = analysis.public.clone();
    if plan.blockers.is_empty() {
        let epoch = build_epoch(fs, analysis, options)?;
        plan.planned_copy_bytes = epoch.copies.iter().map(|c| c.bytes).sum();
        plan.planned_delete_bytes = epoch.deletes.iter().map(|f| f.bytes).sum();
        plan.estimated_net_bytes = i64::try_from(plan.planned_delete_bytes).unwrap_or(i64::MAX)
            - i64::try_from(plan.planned_copy_bytes).unwrap_or(i64::MAX);
    }
    Ok(plan)
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CopyAction {
    source: VaultRelativePath,
    destination: VaultRelativePath,
    hash: Blake3Hash,
    bytes: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Generated {
    path: VaultRelativePath,
    bytes: Vec<u8>,
}
type ExpiryKey = (RecordId, Blake3Hash, VaultRelativePath, Blake3Hash, u64);
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpiredPayload {
    change: PreparedChange,
    reference: PayloadRef,
}
fn expiry_key(change: &PreparedChange, reference: &PayloadRef) -> ExpiryKey {
    (
        change.change_id.clone(),
        change.manifest_hash.clone(),
        reference.path.clone(),
        reference.hash.clone(),
        reference.byte_len,
    )
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompletionReceipt {
    plan_hash: Blake3Hash,
    report: StorageReport,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Epoch {
    version: u32,
    id: RecordId,
    vault_id: RecordId,
    before: StorageTotals,
    original_managed: Vec<StorageFile>,
    retain_undo_changes: usize,
    marker_before: Blake3Hash,
    marker_original: Vec<u8>,
    marker_after: Vec<u8>,
    copies: Vec<CopyAction>,
    generated: Vec<Generated>,
    deletes: Vec<StorageFile>,
    expired_payloads: Vec<ExpiredPayload>,
    protected: Vec<ProtectedStorage>,
}
// Retained engine payloads and journals are at most 64 MiB per action.
// Inventory can measure larger source/unclassified files, but cleanup never
// allocates a larger managed relocation buffer or trusts oversized epoch fields.
fn validate_epoch_bounds(epoch: &Epoch) -> Result<()> {
    const ACTION_LIMIT: u64 = crate::changes::prepare::MAX_PAYLOAD_BYTES as u64;
    const TOTAL_LIMIT: u64 = 16 * 1024 * 1024 * 1024;
    if [
        epoch.copies.len(),
        epoch.deletes.len(),
        epoch.generated.len(),
        epoch.original_managed.len(),
        epoch.expired_payloads.len(),
    ]
    .iter()
    .any(|n| *n > 1_000_000)
    {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "cleanup action count exceeds supported ceiling",
        ));
    }
    for lengths in [
        epoch.copies.iter().map(|c| c.bytes).collect::<Vec<_>>(),
        epoch.deletes.iter().map(|f| f.bytes).collect(),
    ] {
        let mut total = 0u64;
        for length in lengths {
            if length > ACTION_LIMIT {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "cleanup action exceeds supported 64 MiB byte ceiling",
                ));
            }
            total = total
                .checked_add(length)
                .filter(|n| *n <= TOTAL_LIMIT)
                .ok_or_else(|| {
                    WikiError::new(
                        ErrorCode::BudgetExceeded,
                        "cleanup action aggregate exceeds supported 16 GiB ceiling",
                    )
                })?;
        }
    }
    if epoch
        .generated
        .iter()
        .any(|g| g.bytes.len() > crate::changes::prepare::MAX_MANIFEST_BYTES)
    {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "generated payload map exceeds manifest ceiling",
        ));
    }
    Ok(())
}
pub(crate) fn verify_activation(
    root: &crate::vault::VaultRoot,
    activation: &layout::Layout,
) -> Result<()> {
    let result = verify_activation_inner(Some(root), activation, &mut |path, max| {
        layout::raw_read(root, path, max)
    });
    if result.is_err() {
        invalidate_import_epoch(root);
    }
    result
}
pub(crate) fn verify_activation_with_reader(
    activation: &layout::Layout,
    reader: &mut layout::RawReader<'_>,
) -> Result<()> {
    verify_activation_inner(None, activation, reader)
}
fn verify_activation_inner(
    root: Option<&VaultRoot>,
    activation: &layout::Layout,
    reader: &mut layout::RawReader<'_>,
) -> Result<()> {
    let bytes = layout::read_with_reader(reader, &rel(format!(".wiki/state/storage/receipts/{}.plan.json",activation.migration_id))?, MAX_RECEIPT)?
        .ok_or_else(|| err("storage activation lost its original migration plan; restore the complete vault backup"))?;
    if Blake3Hash::digest(&bytes) != activation.migration_hash {
        return Err(err("storage activation migration hash differs"));
    }
    if memoized_epoch(root, activation, false) {
        return Ok(());
    }
    #[cfg(test)]
    import_epoch_memo_tests::decoded_epoch();
    let epoch: Epoch = layout::decode(&bytes)?;
    validate_epoch_bounds(&epoch)?;
    let parsed = crate::records::parse_note(&epoch.marker_original);
    if epoch.version != 1
        || epoch.id != activation.migration_id
        || epoch.vault_id != activation.vault_id
        || !parsed.canonical.as_ref().is_some_and(|r| {
            r.kind() == RecordKind::Vault
                && r.id() == &epoch.vault_id
                && r.string("wiki_schema") == Some("1")
        })
        || Blake3Hash::digest(&epoch.marker_original) != epoch.marker_before
        || crate::records::edit::migrate_schema(&parsed, "2", &epoch.marker_before)?
            != epoch.marker_after
    {
        return Err(err(
            "storage activation lacks exact original schema migration",
        ));
    }
    memoized_epoch(root, activation, true);
    Ok(())
}
pub(crate) fn pending_activation_matches(root: &crate::vault::VaultRoot) -> Result<bool> {
    pending_activation_matches_with_reader(&mut |path, max| layout::raw_read(root, path, max))
}
pub(crate) fn pending_activation_matches_with_reader(
    reader: &mut layout::RawReader<'_>,
) -> Result<bool> {
    let Some(bytes) = layout::read_with_reader(reader, &rel(PENDING)?, MAX_RECEIPT)? else {
        return Ok(false);
    };
    let epoch: Epoch = layout::decode(&bytes)?;
    validate_epoch_bounds(&epoch)?;
    let marker = layout::read_with_reader(reader, &rel("WIKI.md")?, 1024 * 1024)?
        .ok_or_else(|| err("vault marker missing"))?;
    let parsed = crate::records::parse_note(&epoch.marker_original);
    let archive = layout::read_with_reader(
        reader,
        &rel(format!(
            ".wiki/state/storage/receipts/{}.plan.json",
            epoch.id
        ))?,
        MAX_RECEIPT,
    )?;
    Ok(archive.as_ref() == Some(&bytes)
        && parsed.canonical.as_ref().is_some_and(|r| {
            r.kind() == RecordKind::Vault
                && r.id() == &epoch.vault_id
                && r.string("wiki_schema") == Some("1")
        })
        && epoch.version == 1
        && epoch.vault_id == layout::vault_with_reader(reader)?.0
        && Blake3Hash::digest(&epoch.marker_original) == epoch.marker_before
        && marker == epoch.marker_after
        && crate::records::edit::migrate_schema(&parsed, "2", &epoch.marker_before)?
            == epoch.marker_after)
}
fn expiry_warnings(protected: &[ProtectedStorage]) -> Vec<String> {
    if protected
        .iter()
        .any(|p| p.reason.contains("previously_expired"))
    {
        vec!["Increasing the undo window cannot restore previously expired payloads; remaining bytes are protected. Inspect individual changes for current availability.".into()]
    } else {
        vec![]
    }
}
fn completed_expiries(
    fs: &VaultFs,
    inventory: &StorageInventory,
    changes: &[ChangeInspection],
) -> Result<BTreeSet<ExpiryKey>> {
    let mut result = BTreeSet::new();
    let retained: BTreeMap<_, _> = changes
        .iter()
        .map(|c| (c.manifest.change_id.clone(), c))
        .collect();
    for file in inventory.files.iter().filter(|f| {
        f.path.as_str().starts_with(".wiki/state/storage/receipts/")
            && f.path.as_str().ends_with(".plan.json")
    }) {
        let bytes = layout::raw_read(fs.root(), &file.path, MAX_RECEIPT)?
            .ok_or_else(|| err("retained cleanup plan disappeared"))?;
        let epoch: Epoch = layout::decode(&bytes)?;
        validate_epoch_bounds(&epoch)?;
        let receipt_path = rel(format!(".wiki/state/storage/receipts/{}.json", epoch.id))?;
        let Some(receipt_bytes) = layout::raw_read(fs.root(), &receipt_path, MAX_RECEIPT)? else {
            continue;
        };
        let receipt: CompletionReceipt = layout::decode(&receipt_bytes)?;
        if epoch.version != 1
            || epoch.vault_id != inventory.vault_id
            || receipt.report.operation_id != epoch.id
            || file.path
                != rel(format!(
                    ".wiki/state/storage/receipts/{}.plan.json",
                    epoch.id
                ))?
            || receipt.plan_hash != Blake3Hash::digest(&bytes)
            || receipt.report.before != epoch.before
            || receipt.report.deleted_files != epoch.deletes.len() as u64
            || receipt.report.deleted_bytes != epoch.deletes.iter().map(|f| f.bytes).sum::<u64>()
        {
            return Err(err(
                "completed cleanup receipt does not bind exact original plan",
            ));
        }
        for expired in &epoch.expired_payloads {
            let history = retained
                .get(&expired.change.change_id)
                .ok_or_else(|| err("expiry lost original change manifest"))?;
            let object = layout::object_path(&expired.reference.hash)?;
            let original_physical = layout::managed_path(&expired.reference.path)
                .unwrap_or_else(|| expired.reference.path.clone());
            if history.prepared != expired.change
                || permanent_reason(&history.manifest).is_some()
                || !matches!(
                    history.status,
                    ChangeStatus::Committed | ChangeStatus::Aborted
                )
                || !history
                    .manifest
                    .operations
                    .iter()
                    .flat_map(|op| [&op.before_payload, &op.after_payload])
                    .flatten()
                    .any(|p| p == &expired.reference)
                || epoch
                    .protected
                    .iter()
                    .any(|p| p.path.as_str() == format!("changes/{}", expired.change.change_id))
                || epoch.copies.iter().any(|copy| copy.destination == object)
                || !epoch.deletes.iter().any(|file| {
                    (file.path == expired.reference.path
                        || file.path == original_physical
                        || file.path == object)
                        && file.hash == expired.reference.hash
                        && file.bytes == expired.reference.byte_len
                })
            {
                return Err(err(
                    "expired payload lacks exact terminal manifest and deletion identity",
                ));
            }
            result.insert(expiry_key(&expired.change, &expired.reference));
        }
    }
    Ok(result)
}
fn load_epoch(fs: &VaultFs) -> Result<Option<Epoch>> {
    layout::raw_read(fs.root(), &rel(PENDING)?, MAX_RECEIPT)?
        .map(|bytes| {
            let epoch: Epoch = layout::decode(&bytes)?;
            validate_epoch_bounds(&epoch)?;
            Ok(epoch)
        })
        .transpose()
}
fn snapshot_ref(file: &StorageFile, destination: VaultRelativePath) -> CopyAction {
    CopyAction {
        source: file.path.clone(),
        destination,
        hash: file.hash.clone(),
        bytes: file.bytes,
    }
}
fn build_epoch(fs: &VaultFs, analysis: Analysis, options: &StorageOptions) -> Result<Epoch> {
    let current: BTreeMap<_, _> = analysis
        .inventory
        .files
        .iter()
        .map(|file| (file.path.clone(), file))
        .collect();
    let mut copies = BTreeMap::new();
    let mut generated = Vec::new();
    let mut deletes = BTreeMap::new();
    let mut payloads = BTreeMap::<VaultRelativePath, (PayloadRef, bool)>::new();
    let mut live_objects = BTreeSet::new();
    let mut known_objects = BTreeSet::new();
    for change in &analysis.changes {
        let retain = analysis
            .protected_changes
            .contains(&change.manifest.change_id);
        let mut refs = Vec::new();
        for reference in change
            .manifest
            .operations
            .iter()
            .flat_map(|op| [&op.before_payload, &op.after_payload])
            .flatten()
        {
            let object = layout::object_path(&reference.hash)?;
            known_objects.insert(object.clone());
            if retain {
                live_objects.insert(object.clone());
            }
            payloads
                .entry(reference.path.clone())
                .and_modify(|(_, keep)| *keep |= retain)
                .or_insert((reference.clone(), retain));
            refs.push(reference.clone());
            if retain {
                let physical = layout::physical_relative(fs.root(), &reference.path)?;
                let bytes = match layout::raw_read(
                    fs.root(),
                    &physical,
                    crate::changes::prepare::MAX_PAYLOAD_BYTES,
                )? {
                    Some(bytes) => Some(bytes),
                    None => layout::optional_legacy_payload(
                        fs,
                        &reference.path,
                        crate::changes::prepare::MAX_PAYLOAD_BYTES,
                    )?,
                };
                let Some(bytes) = bytes else {
                    if analysis
                        .expired
                        .contains(&expiry_key(&change.prepared, reference))
                    {
                        continue;
                    }
                    return Err(err(
                        "protected retained payload missing without completed expiry proof",
                    ));
                };
                if bytes.len() as u64 != reference.byte_len
                    || Blake3Hash::digest(&bytes) != reference.hash
                {
                    return Err(err("protected payload integrity differs"));
                }
                if reference.path != object {
                    let physical = layout::physical_relative(fs.root(), &reference.path)?;
                    if let Some(file) = current.get(&physical) {
                        copies
                            .entry(object.clone())
                            .or_insert_with(|| snapshot_ref(file, object));
                    }
                    // Already-mapped legacy objects need no additional copy.
                }
            }
        }
        if change.manifest.version == 1 {
            let map = layout::PayloadMap {
                version: 1,
                change_id: change.manifest.change_id.clone(),
                manifest_hash: change.prepared.manifest_hash.clone(),
                entries: refs,
            };
            let path = layout::payload_map_path(&change.manifest.change_id)?;
            let bytes = layout::encode(&map)?;
            if let Some(existing) = layout::raw_read(fs.root(), &path, bytes.len())? {
                if existing != bytes {
                    return Err(err("retained payload mapping changed"));
                }
            } else {
                generated.push(Generated { path, bytes });
            }
        }
    }
    let mirrors: BTreeMap<_, _> = analysis
        .histories
        .iter()
        .flat_map(|h| {
            h.mirrors
                .iter()
                .map(|(p, h, l)| (p.clone(), (h.clone(), *l)))
        })
        .collect();
    for file in &analysis.inventory.files {
        let logical = layout::logical_path(&file.path).unwrap_or_else(|| file.path.clone());
        if let Some((reference, _)) = payloads.get(&logical) {
            // Direct v2 object references are handled by shared reachability below.
            if file.path.as_str().starts_with(".wiki/retained/objects/") {
                continue;
            }
            if file.hash != reference.hash || file.bytes != reference.byte_len {
                return Err(err("retained payload changed during cleanup planning"));
            }
            deletes.insert(file.path.clone(), file.clone());
            continue;
        }
        if mirrors
            .get(&logical)
            .is_some_and(|(hash, len)| hash == &file.hash && *len == file.bytes)
        {
            deletes.insert(file.path.clone(), file.clone());
            continue;
        }
        if let Some(destination) = layout::managed_path(&file.path) {
            copies
                .entry(destination.clone())
                .or_insert_with(|| snapshot_ref(file, destination));
            deletes.insert(file.path.clone(), file.clone());
        }
    }
    for object in known_objects.difference(&live_objects) {
        if let Some(file) = current.get(object) {
            if layout::object_path(&file.hash)? != *object {
                return Err(err("shared object name/hash differs"));
            }
            deletes.insert(object.clone(), (*file).clone());
        }
    }
    let mut expired_payloads = Vec::new();
    for change in &analysis.changes {
        if analysis
            .protected_changes
            .contains(&change.manifest.change_id)
        {
            continue;
        }
        for reference in change
            .manifest
            .operations
            .iter()
            .flat_map(|op| [&op.before_payload, &op.after_payload])
            .flatten()
        {
            let object = layout::object_path(&reference.hash)?;
            if live_objects.contains(&object) {
                continue;
            }
            let physical = layout::physical_relative(fs.root(), &reference.path)?;
            if [&physical, &object].iter().any(|path| {
                deletes.get(*path).is_some_and(|file| {
                    file.hash == reference.hash && file.bytes == reference.byte_len
                })
            }) {
                expired_payloads.push(ExpiredPayload {
                    change: change.prepared.clone(),
                    reference: reference.clone(),
                });
            }
        }
    }
    let marker = layout::raw_read(fs.root(), &rel("WIKI.md")?, 1024 * 1024)?
        .ok_or_else(|| err("vault marker missing"))?;
    let parsed = crate::records::parse_note(&marker);
    let marker_after = if parsed
        .canonical
        .as_ref()
        .and_then(|r| r.string("wiki_schema"))
        == Some("2")
    {
        marker.clone()
    } else {
        crate::records::edit::migrate_schema(&parsed, "2", &Blake3Hash::digest(&marker))?
    };
    let epoch = Epoch {
        version: 1,
        retain_undo_changes: options.retain_undo_changes,
        id: RecordId::new(format!("cleanup_{}", uuid::Uuid::now_v7()))?,
        vault_id: analysis.inventory.vault_id,
        before: analysis.inventory.totals,
        original_managed: analysis
            .inventory
            .files
            .iter()
            .filter(|f| layout::managed_path(&f.path).is_some())
            .cloned()
            .collect(),
        marker_before: Blake3Hash::digest(&marker),
        marker_original: marker,
        marker_after,
        copies: copies.into_values().collect(),
        generated,
        deletes: deletes.into_values().collect(),
        expired_payloads,
        protected: analysis.public.protected,
    };
    validate_epoch_bounds(&epoch)?;
    Ok(epoch)
}
fn remove_exact(
    fs: &VaultFs,
    path: &VaultRelativePath,
    hash: &Blake3Hash,
    length: u64,
) -> Result<()> {
    let Some(bytes) = layout::raw_read(
        fs.root(),
        path,
        usize::try_from(length).map_err(|_| err("delete size overflow"))?,
    )?
    else {
        return Ok(());
    };
    if bytes.len() as u64 != length || Blake3Hash::digest(&bytes) != *hash {
        return Err(WikiError::new(
            ErrorCode::ContentConflict,
            format!("cleanup preserved changed bytes at {path}"),
        ));
    }
    let actual = fs.root().resolve_raw(path)?;
    let io = fs.durable_io();
    io.remove(&actual)
        .map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))?;
    crate::changes::journal::require_sync(
        io.sync_directory(actual.parent().ok_or_else(|| err("delete parent absent"))?)
            .map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))?,
    )
}
fn validate_epoch_actions(fs: &VaultFs, epoch: &Epoch, options: &StorageOptions) -> Result<()> {
    validate_epoch_bounds(epoch)?;
    let marker = layout::raw_read(fs.root(), &rel("WIKI.md")?, 1024 * 1024)?
        .ok_or_else(|| err("vault marker missing"))?;
    if Blake3Hash::digest(&epoch.marker_original) != epoch.marker_before
        || (marker != epoch.marker_original && marker != epoch.marker_after)
    {
        return Err(err("cleanup marker predecessor differs"));
    }
    let parsed = crate::records::parse_note(&epoch.marker_original);
    let original = parsed
        .canonical
        .as_ref()
        .ok_or_else(|| err("cleanup original marker invalid"))?;
    if original.kind() != RecordKind::Vault
        || original.id() != &epoch.vault_id
        || crate::records::edit::migrate_schema(&parsed, "2", &epoch.marker_before)?
            != epoch.marker_after
    {
        return Err(err(
            "cleanup marker replacement is not the exact schema-only migration",
        ));
    }
    let engine = ChangeEngine::new(fs.clone())?;
    let mut payloads = BTreeMap::new();
    let mut maps = BTreeMap::new();
    for id in engine.change_ids()? {
        let history = engine.inspect_history(&id)?;
        let entries: Vec<_> = history
            .manifest
            .operations
            .iter()
            .flat_map(|op| [&op.before_payload, &op.after_payload])
            .flatten()
            .cloned()
            .collect();
        for reference in &entries {
            payloads.insert(reference.path.clone(), reference.clone());
        }
        if history.manifest.version == 1 {
            maps.insert(
                layout::payload_map_path(&id)?,
                layout::encode(&layout::PayloadMap {
                    version: 1,
                    change_id: id,
                    manifest_hash: history.prepared.manifest_hash,
                    entries,
                })?,
            );
        }
    }
    for generated in &epoch.generated {
        if maps.get(&generated.path) != Some(&generated.bytes) {
            return Err(err(
                "cleanup generated bytes lack exact original manifest authority",
            ));
        }
    }
    for copy in &epoch.copies {
        let mapped = layout::managed_path(&copy.source).as_ref() == Some(&copy.destination);
        let logical = layout::logical_path(&copy.source).unwrap_or_else(|| copy.source.clone());
        let object = payloads.get(&logical).is_some_and(|reference| {
            reference.hash == copy.hash
                && reference.byte_len == copy.bytes
                && layout::object_path(&reference.hash).is_ok_and(|path| path == copy.destination)
        });
        if !mapped && !object {
            return Err(err("cleanup copy has no managed path or payload authority"));
        }
        let bytes = layout::raw_read(
            fs.root(),
            &copy.source,
            usize::try_from(copy.bytes).map_err(|_| err("copy size overflow"))?,
        )?
        .or(layout::raw_read(
            fs.root(),
            &copy.destination,
            usize::try_from(copy.bytes).map_err(|_| err("copy size overflow"))?,
        )?);
        if bytes.is_none_or(|bytes| {
            bytes.len() as u64 != copy.bytes || Blake3Hash::digest(bytes) != copy.hash
        }) {
            return Err(err("cleanup copy source/destination no longer matches"));
        }
    }
    let retained = StorageOptions {
        retain_undo_changes: epoch.retain_undo_changes,
        expected_plan: None,
        ..options.clone()
    };
    let analysis = analyze(fs, &retained)?;
    if !analysis.inventory.complete || !analysis.public.blockers.is_empty() {
        return Err(err("cleanup authority inventory incomplete or blocked"));
    }
    for expired in &epoch.expired_payloads {
        let history = analysis
            .changes
            .iter()
            .find(|change| change.prepared == expired.change)
            .ok_or_else(|| err("planned expiry lost exact original change"))?;
        if analysis
            .protected_changes
            .contains(&history.manifest.change_id)
            || !history
                .manifest
                .operations
                .iter()
                .flat_map(|op| [&op.before_payload, &op.after_payload])
                .flatten()
                .any(|r| r == &expired.reference)
        {
            return Err(err("planned expiry has no original unprotected reference"));
        }
        let object = layout::object_path(&expired.reference.hash)?;
        let physical = layout::managed_path(&expired.reference.path)
            .unwrap_or_else(|| expired.reference.path.clone());
        if epoch.copies.iter().any(|copy| copy.destination == object)
            || !epoch.deletes.iter().any(|file| {
                (file.path == expired.reference.path
                    || file.path == physical
                    || file.path == object)
                    && file.hash == expired.reference.hash
                    && file.bytes == expired.reference.byte_len
            })
        {
            return Err(err("planned expiry lacks exact deletion identity"));
        }
    }
    let fresh = build_epoch(fs, analysis, &retained)?;
    let eligible: BTreeMap<_, _> = fresh.deletes.iter().map(|f| (f.path.clone(), f)).collect();
    for file in &epoch.deletes {
        if layout::raw_read(
            fs.root(),
            &file.path,
            usize::try_from(file.bytes).map_err(|_| err("delete size overflow"))?,
        )?
        .is_some()
            && eligible
                .get(&file.path)
                .is_none_or(|current| current.hash != file.hash || current.bytes != file.bytes)
        {
            return Err(err("cleanup deletion has no current retention authority"));
        }
    }
    Ok(())
}
fn resume_epoch(
    fs: &VaultFs,
    writer: &WriterPermit,
    epoch: Epoch,
    resumed: bool,
    options: &StorageOptions,
) -> Result<StorageReport> {
    if epoch.version != 1 || layout::vault(fs.root())?.0 != epoch.vault_id {
        return Err(err("cleanup epoch vault/version differs"));
    }
    validate_epoch_actions(fs, &epoch, options)?;
    let report_path = rel(format!(".wiki/state/storage/receipts/{}.json", epoch.id))?;
    if let Some(bytes) = layout::raw_read(fs.root(), &report_path, MAX_RECEIPT)? {
        let receipt: CompletionReceipt = layout::decode(&bytes)?;
        if receipt.plan_hash != Blake3Hash::digest(layout::encode(&epoch)?) {
            return Err(err("cleanup receipt plan binding differs"));
        }
        let mut report = receipt.report;
        if report.operation_id != epoch.id {
            return Err(err("cleanup receipt identity differs"));
        }
        let bytes = layout::raw_read(fs.root(), &rel(PENDING)?, MAX_RECEIPT)?
            .ok_or_else(|| err("cleanup epoch missing"))?;
        remove_exact(
            fs,
            &rel(PENDING)?,
            &Blake3Hash::digest(&bytes),
            bytes.len() as u64,
        )?;
        report.resumed = true;
        report.after = inventory(fs, options)?.totals;
        return Ok(report);
    }
    let archive = rel(format!(
        ".wiki/state/storage/receipts/{}.plan.json",
        epoch.id
    ))?;
    let epoch_bytes = layout::encode(&epoch)?;
    layout::put(fs, writer, &archive, &epoch_bytes)?;
    if !layout::active(fs.root())? {
        // Ordinary mutations are blocked by the durable pending epoch. Also
        // detect out-of-process edits/new legacy files before switching readers.
        let now = inventory(fs, options)?;
        if !now.complete {
            return Err(err("resume inventory incomplete"));
        }
        let managed: Vec<_> = now
            .files
            .into_iter()
            .filter(|f| layout::managed_path(&f.path).is_some())
            .collect();
        if managed != epoch.original_managed {
            return Err(err(
                "legacy managed files changed during migration; restore the complete pre-cleanup backup or restore the listed exact bytes before resuming",
            ));
        }
    }
    for copy in &epoch.copies {
        if let Some(bytes) = layout::raw_read(
            fs.root(),
            &copy.destination,
            usize::try_from(copy.bytes).map_err(|_| err("copy size overflow"))?,
        )? {
            if bytes.len() as u64 != copy.bytes || Blake3Hash::digest(&bytes) != copy.hash {
                return Err(err("migration destination has unfamiliar bytes"));
            }
            crate::changes::journal::require_sync(fs.sync_target(&copy.destination, writer)?)?;
            continue;
        }
        let bytes = layout::raw_read(
            fs.root(),
            &copy.source,
            usize::try_from(copy.bytes).map_err(|_| err("copy size overflow"))?,
        )?
        .ok_or_else(|| err("migration source disappeared"))?;
        if bytes.len() as u64 != copy.bytes || Blake3Hash::digest(&bytes) != copy.hash {
            return Err(err("migration source changed"));
        }
        layout::put(fs, writer, &copy.destination, &bytes)?;
    }
    for generated in &epoch.generated {
        layout::put(fs, writer, &generated.path, &generated.bytes)?;
    }
    // Upgrade the marker only after all backing bytes exist. Old tools refuse
    // schema2 before activation; a crash here is safely resumed using this epoch.
    let marker_path = rel("WIKI.md")?;
    let marker = layout::raw_read(fs.root(), &marker_path, 1024 * 1024)?
        .ok_or_else(|| err("vault marker disappeared"))?;
    if marker != epoch.marker_after {
        if Blake3Hash::digest(&marker) != epoch.marker_before {
            return Err(err("vault marker changed during storage transition"));
        }
        let staged = fs.stage(&marker_path, &epoch.marker_after, writer)?;
        crate::changes::journal::require_sync(fs.replace(
            staged,
            &ExpectedState::Hash(epoch.marker_before.clone()),
            writer,
        )?)?;
    } else {
        crate::changes::journal::require_sync(fs.sync_target(&marker_path, writer)?)?;
    }
    if !layout::active(fs.root())? {
        layout::put(
            fs,
            writer,
            &rel(layout::ACTIVE)?,
            &layout::encode(&layout::Layout {
                version: 2,
                vault_id: epoch.vault_id.clone(),
                migration_id: epoch.id.clone(),
                migration_hash: Blake3Hash::digest(&epoch_bytes),
            })?,
        )?;
    }
    // Reopen exact retained authority through the active mapping before unlink.
    let engine = ChangeEngine::new(fs.clone())?;
    for id in engine.change_ids()? {
        engine.inspect_history(&id)?;
    }
    for run in existing_runs(fs, &epoch.vault_id)? {
        checkpoint::storage_run_history(fs, &run)?;
    }
    // A checksum only detects damage; it does not authorize deleting a path.
    // Re-derive reachability under the original policy and currently locked
    // authority roots on every resume, including after partial unlink.
    let retained_options = StorageOptions {
        retain_undo_changes: epoch.retain_undo_changes,
        expected_plan: None,
        ..options.clone()
    };
    let current_analysis = analyze(fs, &retained_options)?;
    if !current_analysis.public.blockers.is_empty() {
        return Err(err("cleanup authority inventory is incomplete"));
    }
    let current_epoch = build_epoch(fs, current_analysis, &retained_options)?;
    let eligible: BTreeMap<_, _> = current_epoch
        .deletes
        .iter()
        .map(|f| (f.path.clone(), f))
        .collect();
    for file in &epoch.deletes {
        if layout::raw_read(
            fs.root(),
            &file.path,
            usize::try_from(file.bytes).map_err(|_| err("delete size overflow"))?,
        )?
        .is_none()
        {
            continue;
        }
        if eligible
            .get(&file.path)
            .is_none_or(|current| current.hash != file.hash || current.bytes != file.bytes)
        {
            return Err(err(
                "cleanup deletion is no longer eligible; protected bytes preserved",
            ));
        }
        remove_exact(fs, &file.path, &file.hash, file.bytes)?;
    }
    let mut directories = BTreeSet::new();
    for file in &epoch.deletes {
        if layout::managed_path(&file.path).is_none() {
            continue;
        }
        let mut parent = file.path.as_str().rsplit_once('/').map(|(p, _)| p);
        while let Some(value) = parent {
            let path = rel(value)?;
            if layout::managed_path(&path).is_none() {
                break;
            }
            directories.insert(path);
            parent = value.rsplit_once('/').map(|(p, _)| p);
        }
    }
    for directory in directories.iter().rev() {
        let path = fs.root().resolve_raw(directory)?;
        match std::fs::read_dir(&path) {
            Ok(mut entries) => {
                if entries.next().is_some() {
                    continue;
                }
                let io = fs.durable_io();
                io.remove_directory(&path)
                    .map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))?;
                crate::changes::journal::require_sync(
                    io.sync_directory(
                        path.parent()
                            .ok_or_else(|| err("directory parent missing"))?,
                    )
                    .map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))?,
                )?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(WikiError::new(ErrorCode::Internal, e.to_string())),
        }
    }
    let navigation = rel("STORAGE.md")?;
    let guide=b"# Operational storage\n\nPages, source revisions, evidence and human reports remain ordinary Markdown. Detailed records live under `.wiki/retained`; their original logical paths still work with `lwiki read --path PATH`. For example, `lwiki read --path runs/RUN/run.md` opens a run summary. `lwiki changes show CHANGE` shows retained change history; `lwiki jobs status --run RUN` inspects accounting and holds. Operational paths are logical application paths, not ordinary visible Markdown links. Use `lwiki storage inventory` before cleanup.\n\nBack up the **entire vault, including `.wiki`**. A visible-Markdown-only copy does not preserve resumable accounting or undo. Old lwiki versions refuse this vault format; downgrade requires the complete pre-upgrade backup. Source revisions are never removed by storage cleanup.\n";
    if layout::raw_read(fs.root(), &navigation, 1024 * 1024)?.is_none() {
        layout::put(fs, writer, &navigation, guide)?;
    }
    let after = inventory(fs, options)?;
    if !after.complete {
        return Err(err(
            "post-cleanup inventory incomplete; epoch retained for resume",
        ));
    }
    let mut report=StorageReport{operation_id:epoch.id,resumed,layout_version:2,before:epoch.before,after:after.totals,
        deleted_files:epoch.deletes.len() as u64,deleted_bytes:epoch.deletes.iter().map(|f|f.bytes).sum(),
        copied_files:epoch.copies.len() as u64,copied_bytes:epoch.copies.iter().map(|f|f.bytes).sum(),protected:epoch.protected.clone(),warnings:expiry_warnings(&epoch.protected),
        backup:"Full vault backup including .wiki is required; knowledge-only copies cannot resume old accounting; downgrade restores the pre-upgrade full backup.".into()};
    let receipt = layout::encode(&CompletionReceipt {
        plan_hash: Blake3Hash::digest(&epoch_bytes),
        report: report.clone(),
    })?;
    if receipt.len() > MAX_RECEIPT {
        return Err(err("cleanup receipt exceeds bound"));
    }
    layout::put(fs, writer, &report_path, &receipt)?;
    let pending = layout::raw_read(fs.root(), &rel(PENDING)?, MAX_RECEIPT)?
        .ok_or_else(|| err("cleanup epoch disappeared"))?;
    remove_exact(
        fs,
        &rel(PENDING)?,
        &Blake3Hash::digest(&pending),
        pending.len() as u64,
    )?;
    report.after = inventory(fs, options)?.totals;
    Ok(report)
}
pub fn cleanup(
    fs: &VaultFs,
    writer: &WriterPermit,
    options: &StorageOptions,
) -> Result<StorageReport> {
    validate(options)?;
    writer.require_root(fs.root())?;
    let recovery_fs = fs.for_storage_recovery();
    let fs = &recovery_fs;
    let (vault_id, _) = layout::vault(fs.root())?;
    // Universal existing writer→run order. No job may append while the roots
    // are checked and unlinked; new job creation also needs this writer permit.
    let stores = existing_runs(fs, &vault_id)?
        .iter()
        .map(|id| RunStore::open_existing(fs, &vault_id, id))
        .collect::<Result<Vec<_>>>()?;
    let _guards = stores
        .iter()
        .map(|store| store.lock(Duration::from_secs(5), &mut || Ok(())))
        .collect::<Result<Vec<_>>>()?;
    if let Some(epoch) = load_epoch(fs)? {
        return resume_epoch(fs, writer, epoch, true, options);
    }
    let initial = analyze(fs, options)?;
    if options
        .expected_plan
        .as_ref()
        .is_some_and(|hash| hash != &initial.public.plan_hash)
    {
        return Err(WikiError::new(
            ErrorCode::ContentConflict,
            "storage plan changed; preview again",
        ));
    }
    if !initial.inventory.complete {
        return Err(err("cleanup inventory incomplete"));
    }
    let engine = ChangeEngine::new(fs.clone())?;
    engine.recover(
        writer,
        &CatalogGraphValidator,
        &Catalog::new(fs.clone(), vault_id.clone()),
    )?;
    // A compact-proof publication may have stopped before the storage epoch
    // existed. Normal engine recovery finishes its original authorized intent.
    let recovered = analyze(fs, options)?;
    if !recovered.public.blockers.is_empty() {
        return Err(err(&recovered.public.blockers.join("; ")));
    }
    for history in &recovered.histories {
        for (run, summary) in &history.checkpoints {
            let draft =
                checkpoint::compact_summary_migration_plan(fs, run, summary, &history.frames)?;
            if draft.operations.is_empty() {
                continue;
            }
            let prepared = engine.prepare(writer, draft)?.prepared;
            engine.apply(
                writer,
                &prepared,
                &CatalogGraphValidator,
                &Catalog::new(fs.clone(), vault_id.clone()),
            )?;
        }
    }
    let refreshed = analyze(fs, options)?;
    let mut epoch = build_epoch(fs, refreshed, options)?;
    epoch.before = initial.inventory.totals;
    if layout::active(fs.root())?
        && epoch.copies.is_empty()
        && epoch.deletes.is_empty()
        && epoch.generated.iter().all(|g| {
            layout::raw_read(fs.root(), &g.path, g.bytes.len())
                .ok()
                .flatten()
                .as_ref()
                == Some(&g.bytes)
        })
    {
        return Ok(StorageReport {
            operation_id: RecordId::new("cleanup_noop")?,
            resumed: false,
            layout_version: 2,
            before: epoch.before.clone(),
            after: epoch.before,
            deleted_files: 0,
            deleted_bytes: 0,
            copied_files: 0,
            copied_bytes: 0,
            warnings: expiry_warnings(&epoch.protected),
            protected: epoch.protected,
            backup: "Back up the complete vault including .wiki.".into(),
        });
    }
    let encoded = layout::encode(&epoch)?;
    if encoded.len() > MAX_RECEIPT {
        return Err(err("cleanup epoch exceeds bounded receipt size"));
    }
    layout::put(fs, writer, &rel(PENDING)?, &encoded)?;
    resume_epoch(fs, writer, epoch, false, options)
}

#[cfg(test)]
#[path = "import_epoch_memo_tests.rs"]
mod import_epoch_memo_tests;

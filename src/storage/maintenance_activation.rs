//! Private semantic proof of exact owner-validated activation inputs.
//! Workers freshly authenticate each witness; they never decode authority.
use super::layout::{self, Layout, ValidatedLayout};
use crate::{
    domain::{ErrorCode, Result, VaultRelativePath, WikiError},
    vault::{VaultFs, VaultRoot, WriterPermit},
};
use std::{cell::RefCell, collections::BTreeMap, mem::size_of, sync::Arc};

pub(crate) const WORKSPACE_BYTES: u64 = 72 * 1024;
const _: () = assert!(size_of::<blake3::Hasher>() <= 8 * 1024);
struct Witness {
    path: VaultRelativePath,
    len: u64,
    hash: [u8; 32],
    limit: usize,
}
pub(crate) struct ActivationProof {
    root: VaultRoot,
    witnesses: [Witness; 3],
}
struct OwnerState {
    root: VaultRoot,
    proof: Option<Arc<ActivationProof>>,
}
thread_local! {
    static OWNER: RefCell<Option<OwnerState>> = const { RefCell::new(None) };
    static WORKER: RefCell<Option<Option<Arc<ActivationProof>>>> = const { RefCell::new(None) };
    static OWNER_CAPTURE: RefCell<bool> = const { RefCell::new(false) };
}
fn conflict(message: &str) -> WikiError {
    WikiError::new(ErrorCode::ContentConflict, message)
}
fn admission(message: &str) -> WikiError {
    WikiError::new(ErrorCode::Internal, message)
}
pub(crate) fn owner_capture_active() -> bool {
    OWNER_CAPTURE.with(|slot| *slot.borrow())
}
struct OwnerCaptureGuard;
impl OwnerCaptureGuard {
    fn enter() -> Result<Self> {
        OWNER_CAPTURE.with(|slot| {
            let mut slot = slot.borrow_mut();
            if *slot {
                return Err(admission("nested owner activation capture"));
            }
            *slot = true;
            Ok(Self)
        })
    }
}
impl Drop for OwnerCaptureGuard {
    fn drop(&mut self) {
        OWNER_CAPTURE.with(|slot| *slot.borrow_mut() = false);
    }
}

/// Called only under the same command owner. Unsupported modes select the
/// sequential operation interface before dispatch; a failed validation refuses.
pub(crate) fn prepare_parallel_activation(
    permit: &WriterPermit,
    fs: &VaultFs,
) -> Result<Option<Arc<ActivationProof>>> {
    permit.require_root(fs.root())?;
    if !crate::maintenance_parallel::parallel_enabled() {
        return Ok(None);
    }
    if WORKER.with(|slot| slot.borrow().is_some()) {
        return Err(admission("worker cannot construct activation proof"));
    }
    if fs.is_storage_recovery()
        || super::import_epoch_active(fs.root())
        || fs
            .root()
            .resolve_raw(&VaultRelativePath::new(".wiki/state/storage/cleanup.json")?)?
            .exists()
    {
        return Ok(None);
    }
    if let Some(proof) = OWNER.with(|slot| {
        slot.borrow()
            .as_ref()
            .filter(|state| &state.root == fs.root())
            .map(|state| state.proof.clone())
    }) {
        return Ok(proof);
    }
    let proof = capture(fs.root())?;
    let state = OwnerState {
        root: fs.root().clone(),
        proof: proof.clone(),
    };
    let owned = size_of::<OwnerState>() as u64
        + state.root.owned_capacity() as u64
        + proof.as_ref().map_or(0, |proof| proof.owned_bytes());
    crate::maintenance_parallel::reserve_shared(owned)?;
    OWNER.with(|slot| *slot.borrow_mut() = Some(state));
    Ok(proof)
}
fn capture(root: &VaultRoot) -> Result<Option<Arc<ActivationProof>>> {
    let mut observed = BTreeMap::<VaultRelativePath, Witness>::new();
    let mut activation = None;
    let mut shape_valid = true;
    let validated = {
        let _capture = OwnerCaptureGuard::enter()?;
        crate::maintenance_parallel::record_owner_activation_validation();
        ValidatedLayout::capture_with_reader(root, &mut |path, limit| {
            let bytes = layout::raw_read(root, path, limit)?;
            if let Some(bytes) = &bytes {
                if path.as_str() == layout::ACTIVE {
                    activation = Some(layout::decode::<Layout>(bytes)?);
                }
                let witness = Witness {
                    path: path.clone(),
                    len: bytes.len() as u64,
                    hash: *blake3::hash(bytes).as_bytes(),
                    limit,
                };
                if observed.insert(path.clone(), witness).is_some() {
                    shape_valid = false;
                }
            } else {
                shape_valid = false;
            }
            Ok(bytes)
        })
    }?;
    if !validated.retained(root)? || !shape_valid || observed.len() != 3 {
        return Ok(None);
    }
    let Some(activation) = activation else {
        return Ok(None);
    };
    let receipt = VaultRelativePath::new(format!(
        ".wiki/state/storage/receipts/{}.plan.json",
        activation.migration_id
    ))?;
    let paths = [
        (layout::ACTIVE, 4096),
        ("WIKI.md", 1024 * 1024),
        (receipt.as_str(), 64 * 1024 * 1024),
    ];
    let mut witnesses = Vec::with_capacity(3);
    for (path, limit) in paths {
        let Some(witness) = observed.remove(&VaultRelativePath::new(path)?) else {
            return Ok(None);
        };
        if witness.limit != limit {
            return Ok(None);
        }
        witnesses.push(witness);
    }
    let proof = ActivationProof {
        root: root.clone(),
        witnesses: witnesses
            .try_into()
            .map_err(|_| admission("activation witness shape changed"))?,
    };
    if proof.path_workspace().is_none() {
        return Ok(None);
    }
    Ok(Some(Arc::new(proof)))
}
impl ActivationProof {
    /// Account the single shared proof allocation, owned paths/root and Arc words.
    fn owned_bytes(&self) -> u64 {
        (size_of::<Self>()
            + 2 * size_of::<usize>()
            + self.root.owned_capacity()
            + self
                .witnesses
                .iter()
                .map(|witness| witness.path.owned_capacity())
                .sum::<usize>()) as u64
    }
    fn active(&self, root: &VaultRoot) -> Result<bool> {
        if &self.root != root {
            return Err(conflict(
                "activation proof belongs to a different vault root",
            ));
        }
        for witness in &self.witnesses {
            let actual = crate::vault::fs::hash_regular_raw_bounded(
                root,
                &witness.path,
                witness.limit,
                "storage-read",
            )
            .map_err(|error| {
                conflict(&format!(
                    "activation authority observation failed: {}",
                    error.message
                ))
            })?;
            if actual != Some((witness.len, witness.hash)) {
                return Err(conflict(
                    "activation authority bytes changed during parallel phase",
                ));
            }
        }
        Ok(true)
    }
    pub(crate) fn path_workspace(&self) -> Option<u64> {
        self.witnesses
            .iter()
            .map(|witness| crate::vault::paths::parallel_path_workspace(&self.root, &witness.path))
            .try_fold(0u64, |maximum, workspace| {
                workspace.map(|workspace| maximum.max(workspace))
            })
    }
}
pub(crate) fn current_proof() -> Option<Arc<ActivationProof>> {
    OWNER.with(|slot| slot.borrow().as_ref().and_then(|state| state.proof.clone()))
}
pub(crate) fn clear_owner() {
    OWNER.with(|slot| slot.borrow_mut().take());
}
pub(crate) fn worker_context_active() -> bool {
    WORKER.with(|slot| slot.borrow().is_some())
}
pub(crate) struct WorkerGuard;
pub(crate) fn enter_worker(proof: Option<Arc<ActivationProof>>) -> Result<WorkerGuard> {
    WORKER.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_some() {
            return Err(admission("nested activation worker proof"));
        }
        *slot = Some(proof);
        Ok(WorkerGuard)
    })
}
impl Drop for WorkerGuard {
    fn drop(&mut self) {
        WORKER.with(|slot| slot.borrow_mut().take());
    }
}
/// None means owner execution; Some(result) is an admitted worker observation.
pub(crate) fn worker_active(root: &VaultRoot) -> Option<Result<bool>> {
    WORKER.with(|slot| {
        slot.borrow().as_ref().map(|proof| match proof {
            Some(proof) => proof.active(root),
            None => Err(admission(
                "parallel worker reached activation without owner proof",
            )),
        })
    })
}

#[cfg(test)]
#[path = "maintenance_activation_tests.rs"]
mod tests;

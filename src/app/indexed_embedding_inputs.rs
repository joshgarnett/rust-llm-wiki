//! Normalized preparation inputs with retained, bounded single-owner proofs.
//! No legacy projection, canonical auto-scan, provider access or cache writes.
use crate::{
    catalog::{
        Catalog,
        query::QuerySnapshot,
        query_types::{QueryCatalog, QueryReadLimits},
    },
    changes::ReadDependency,
    domain::*,
    retrieval::{
        context_types::VerificationBudget,
        indexed_units::{self, AuthenticatedDocument, UnitBudget, UnitLimits},
        render::RenderedUnit,
        spaces::EmbeddingSettings,
    },
    vault::ExpectedState,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    mem::size_of,
    sync::Arc,
    time::{Duration, Instant},
};

pub(super) type InputBindings = BTreeMap<Blake3Hash, Arc<[ReadDependency]>>;

/// A newly declared paid scope. Reuse acknowledgment still authenticates all
/// owners; these units and guards identify only the suppliers of paid inputs.
pub(super) struct PaidEmbeddingScope {
    pub units: Vec<RenderedUnit>,
    pub source_bindings: InputBindings,
}

const MAX_OWNERS: usize = 4096;
const MAX_PATH_BYTES: usize = 8 * 1024 * 1024;
const MAX_BINDING_ENTRIES: usize = 65_536;
const MAX_BINDING_BYTES: usize = 64 * 1024 * 1024;

/// Test-build scalar attribution only; shipping preparation has no observer.
#[cfg(test)]
pub(super) mod attribution {
    use std::{cell::RefCell, time::Instant};

    #[derive(Default, serde::Serialize)]
    pub(in crate::app) struct Observation {
        pub materialize_calls: usize,
        pub materialize_elapsed_ns: u128,
        pub recheck_calls: usize,
        pub recheck_elapsed_ns: u128,
        pub owner_attempts: usize,
        pub authenticated_owners: usize,
        pub rendered_units: usize,
        pub rendered_bytes: usize,
        pub cache_rows: usize,
        pub cache_bytes: usize,
        pub canonical_bytes: usize,
        pub canonical_files: usize,
        pub canonical_entries: usize,
        pub incomplete_stages: usize,
        pub unavailable_failed_owner_proof: usize,
        pub prior_accounting_discovery_calls: usize,
    }

    std::thread_local! {
        static ACTIVE: RefCell<Option<Observation>> = const { RefCell::new(None) };
    }

    pub(in crate::app) fn with_observation<T>(run: impl FnOnce() -> T) -> (T, Observation) {
        ACTIVE.with(|active| {
            assert!(active.borrow().is_none(), "nested preparation attribution");
            *active.borrow_mut() = Some(Observation::default());
        });
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                ACTIVE.with(|active| *active.borrow_mut() = None);
            }
        }
        let reset = Reset;
        let result = run();
        let observation = ACTIVE.with(|active| active.borrow_mut().take().unwrap());
        drop(reset);
        (result, observation)
    }

    fn update(run: impl FnOnce(&mut Observation)) {
        ACTIVE.with(|active| {
            if let Some(observation) = active.borrow_mut().as_mut() {
                run(observation);
            }
        });
    }

    pub(super) struct Stage {
        start: Instant,
        recheck: bool,
        pub complete: bool,
    }
    impl Stage {
        pub(super) fn new(recheck: bool) -> Self {
            Self {
                start: Instant::now(),
                recheck,
                complete: false,
            }
        }
    }
    impl Drop for Stage {
        fn drop(&mut self) {
            update(|observation| {
                if self.recheck {
                    observation.recheck_calls += 1;
                    observation.recheck_elapsed_ns += self.start.elapsed().as_nanos();
                } else {
                    observation.materialize_calls += 1;
                    observation.materialize_elapsed_ns += self.start.elapsed().as_nanos();
                }
                observation.incomplete_stages += usize::from(!self.complete);
            });
        }
    }

    pub(super) fn cache(rows: usize, bytes: usize) {
        update(|o| {
            o.cache_rows += rows;
            o.cache_bytes += bytes;
        });
    }
    pub(super) fn owner(units: usize, bytes: usize, proof: Option<(usize, usize, usize)>) {
        update(|o| {
            o.owner_attempts += 1;
            o.rendered_units += units;
            o.rendered_bytes += bytes;
            if let Some(work) = proof {
                o.authenticated_owners += 1;
                o.canonical_bytes += work.0;
                o.canonical_files += work.1;
                o.canonical_entries += work.2;
            } else {
                o.unavailable_failed_owner_proof += 1;
            }
        });
    }
    pub(super) fn proof(work: (usize, usize, usize)) {
        update(|o| {
            o.canonical_bytes += work.0;
            o.canonical_files += work.1;
            o.canonical_entries += work.2;
        });
    }
    pub(in crate::app) fn prior_accounting_discovery() {
        update(|observation| observation.prior_accounting_discovery_calls += 1);
    }
}
fn exhausted(message: &str) -> WikiError {
    WikiError::new(ErrorCode::BudgetExceeded, message)
}
fn conflict(message: &str) -> WikiError {
    WikiError::new(ErrorCode::FreshnessConflict, message)
}

struct Allowance {
    remaining: VerificationBudget,
    deadline: Instant,
}
impl Allowance {
    fn new(budget: &VerificationBudget) -> Result<Self> {
        let start = Instant::now();
        if budget.max_bytes == 0
            || budget.max_bytes > 256 * 1024 * 1024
            || budget.max_files == 0
            || budget.max_files > 16384
            || budget.max_entries == 0
            || budget.max_entries > 65536
            || budget.max_elapsed_ms == 0
            || budget.max_elapsed_ms > 30_000
        {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "normalized embedding proof budget exceeds its ceiling",
            ));
        }
        Ok(Self {
            remaining: budget.clone(),
            deadline: start + Duration::from_millis(budget.max_elapsed_ms),
        })
    }
    fn milliseconds(&self) -> Result<u64> {
        let remaining = self
            .deadline
            .saturating_duration_since(Instant::now())
            .as_millis();
        let remaining = u64::try_from(remaining).unwrap_or(u64::MAX);
        if remaining == 0 {
            return Err(exhausted("normalized embedding input deadline exceeded"));
        }
        Ok(remaining)
    }
    fn budget(&self) -> Result<VerificationBudget> {
        if self.remaining.max_bytes == 0
            || self.remaining.max_files == 0
            || self.remaining.max_entries == 0
        {
            return Err(exhausted(
                "normalized embedding aggregate proof allowance exhausted",
            ));
        }
        let mut budget = self.remaining.clone();
        budget.max_elapsed_ms = self.milliseconds()?;
        Ok(budget)
    }
    fn debit(&mut self, work: (usize, usize, usize)) -> Result<()> {
        let bytes = self.remaining.max_bytes.checked_sub(work.0);
        let files = self.remaining.max_files.checked_sub(work.1);
        let entries = self.remaining.max_entries.checked_sub(work.2);
        if bytes.is_none() || files.is_none() || entries.is_none() {
            return Err(exhausted(
                "normalized embedding proof work exceeded aggregate allowance",
            ));
        }
        self.remaining.max_bytes = bytes.unwrap();
        self.remaining.max_files = files.unwrap();
        self.remaining.max_entries = entries.unwrap();
        Ok(())
    }
}
#[derive(Clone, Default)]
struct BindingInventory {
    entries: usize,
    bytes: usize,
}
impl BindingInventory {
    fn reserve(&mut self, entries: usize, bytes: usize) -> Result<()> {
        let entries = self
            .entries
            .checked_add(entries)
            .filter(|n| *n <= MAX_BINDING_ENTRIES);
        let bytes = self
            .bytes
            .checked_add(bytes)
            .filter(|n| *n <= MAX_BINDING_BYTES);
        if entries.is_none() || bytes.is_none() {
            return Err(exhausted(
                "normalized embedding source binding inventory exhausted",
            ));
        }
        self.entries = entries.unwrap();
        self.bytes = bytes.unwrap();
        Ok(())
    }
}
fn guard_bytes(path: &VaultRelativePath, expected: &ExpectedState) -> Result<usize> {
    let expected_bytes = match expected {
        ExpectedState::Absent => 0,
        ExpectedState::Hash(hash) => hash.as_str().len(),
    };
    size_of::<ReadDependency>()
        .checked_add(path.as_str().len())
        .and_then(|bytes| bytes.checked_add(expected_bytes))
        .ok_or_else(|| exhausted("normalized embedding guard byte count overflow"))
}

fn retain_state<'a>(
    states: &mut BTreeMap<&'a VaultRelativePath, &'a ExpectedState>,
    inventory: &mut BindingInventory,
    path: &'a VaultRelativePath,
    expected: &'a ExpectedState,
) -> Result<()> {
    if let Some(previous) = states.get(path) {
        if *previous != expected {
            return Err(conflict("embedding owners have conflicting read guards"));
        }
    } else {
        inventory.reserve(1, size_of::<(&VaultRelativePath, &ExpectedState)>())?;
        states.insert(path, expected);
    }
    Ok(())
}

pub(super) struct NormalizedEmbeddingInputs {
    pub snapshot: ReadSnapshot,
    /// All exact owner units remain present, including shared request inputs.
    pub units: Vec<RenderedUnit>,
    /// Compact exact input-to-owner incidences; complete guards live in owners.
    input_owners: BTreeMap<Blake3Hash, Vec<usize>>,
    owners: Vec<AuthenticatedDocument>,
    reader: QuerySnapshot,
    allowance: Allowance,
    inventory: BindingInventory,
    failed: bool,
}
impl NormalizedEmbeddingInputs {
    pub(super) fn paid_scope(
        &self,
        units: &[RenderedUnit],
        missing: &[crate::providers::types::EmbeddingInput],
        available_guards: usize,
    ) -> Result<PaidEmbeddingScope> {
        self.allowance.milliseconds()?;
        if self.failed {
            return Err(conflict(
                "cannot plan paid inputs from a failed owner proof",
            ));
        }
        let mut inventory = BindingInventory::default();
        let mut wanted = BTreeSet::new();
        for input in missing {
            self.allowance.milliseconds()?;
            if !wanted.contains(&input.input_hash) {
                inventory.reserve(1, size_of::<&Blake3Hash>())?;
                wanted.insert(&input.input_hash);
            }
        }
        let mut by_owner: BTreeMap<_, BTreeMap<_, Vec<_>>> = BTreeMap::new();
        for unit in units
            .iter()
            .filter(|unit| wanted.contains(&unit.input_hash))
        {
            self.allowance.milliseconds()?;
            if !by_owner.contains_key(&unit.owner) {
                inventory.reserve(
                    1,
                    size_of::<(
                        &VaultRelativePath,
                        BTreeMap<&Blake3Hash, Vec<&RenderedUnit>>,
                    )>(),
                )?;
                by_owner.insert(&unit.owner, BTreeMap::new());
            }
            let hashes = by_owner.get_mut(&unit.owner).expect("reserved paid owner");
            if !hashes.contains_key(&unit.input_hash) {
                inventory.reserve(1, size_of::<(&Blake3Hash, Vec<&RenderedUnit>)>())?;
                hashes.insert(&unit.input_hash, Vec::new());
            }
            inventory.reserve(1, size_of::<&RenderedUnit>())?;
            let selected = hashes
                .get_mut(&unit.input_hash)
                .expect("reserved paid input");
            selected
                .try_reserve_exact(1)
                .map_err(|_| exhausted("paid unit reference allocation refused"))?;
            selected.push(unit);
        }
        let mut suppliers = BTreeMap::new();
        let mut oversized: BTreeMap<&Blake3Hash, (&VaultRelativePath, usize)> = BTreeMap::new();
        // Choice maps borrow hashes and paths. Count their entries before retaining
        // them; oversized proofs are never expanded to owned guards.
        for (index, owner) in self.owners.iter().enumerate() {
            self.allowance.milliseconds()?;
            let path = owner
                .proof
                .documents
                .keys()
                .next()
                .ok_or_else(|| conflict("authenticated embedding supplier disappeared"))?;
            let Some(owner_units) = by_owner.get(path) else {
                continue;
            };
            let mut required = 0usize;
            for _ in owner.proof.read_preconditions_iter() {
                self.allowance.milliseconds()?;
                required += 1;
            }
            for hash in owner_units.keys().copied() {
                self.allowance.milliseconds()?;
                if required <= available_guards {
                    if !suppliers.contains_key(hash) {
                        inventory.reserve(1, size_of::<(&Blake3Hash, usize)>())?;
                        suppliers.insert(hash, index);
                    }
                } else if let Some(candidate) = oversized.get_mut(hash) {
                    if required < candidate.1 {
                        *candidate = (path, required);
                    }
                } else {
                    inventory
                        .reserve(1, size_of::<(&Blake3Hash, (&VaultRelativePath, usize))>())?;
                    oversized.insert(hash, (path, required));
                }
            }
        }
        let mut scope = PaidEmbeddingScope {
            units: Vec::new(),
            source_bindings: BTreeMap::new(),
        };
        let mut shared: BTreeMap<usize, Arc<[ReadDependency]>> = BTreeMap::new();
        for input in missing {
            self.allowance.milliseconds()?;
            if scope.source_bindings.contains_key(&input.input_hash) {
                continue;
            }
            let Some(&index) = suppliers.get(&input.input_hash) else {
                if let Some((owner, required)) = oversized.get(&input.input_hash) {
                    let mut error = exhausted(&format!(
                        "embedding input {} supplied by {} requires {} source guards; only {} are available (one additional Run guard is reserved)",
                        input.input_hash, owner, required, available_guards,
                    ));
                    error.details = serde_json::json!({"input_hash":input.input_hash,"owner":owner,
                        "required_source_guards":required,"available_source_guards":available_guards,
                        "reserved_run_guards":1});
                    return Err(error);
                }
                return Err(conflict(
                    "missing embedding input has no authenticated supplier",
                ));
            };
            // Every key and Arc handle is accounted before either allocation.
            inventory.reserve(
                1,
                size_of::<(Blake3Hash, Arc<[ReadDependency]>)>()
                    .checked_add(input.input_hash.as_str().len())
                    .ok_or_else(|| exhausted("paid input key byte overflow"))?,
            )?;
            if !shared.contains_key(&index) {
                inventory.reserve(
                    1,
                    size_of::<(usize, Arc<[ReadDependency]>)>() + 2 * size_of::<usize>(),
                )?;
                let owner = &self.owners[index];
                for (path, expected) in owner.proof.read_preconditions_iter() {
                    self.allowance.milliseconds()?;
                    inventory.reserve(1, guard_bytes(path, expected)?)?;
                }
                let mut guards = Vec::new();
                guards
                    .try_reserve_exact(owner.proof.read_preconditions_iter().count())
                    .map_err(|_| exhausted("paid supplier guard allocation refused"))?;
                for (path, expected) in owner.proof.read_preconditions_iter() {
                    self.allowance.milliseconds()?;
                    guards.push(ReadDependency {
                        path: path.clone(),
                        expected: expected.clone(),
                    });
                }
                shared.insert(index, Arc::from(guards));
            }
            let owner_path = self.owners[index]
                .proof
                .documents
                .keys()
                .next()
                .ok_or_else(|| conflict("authenticated embedding supplier disappeared"))?;
            let selected = &by_owner[owner_path][&input.input_hash];
            scope
                .units
                .try_reserve_exact(selected.len())
                .map_err(|_| exhausted("paid supplier unit allocation refused"))?;
            scope
                .units
                .extend(selected.iter().map(|unit| (**unit).clone()));
            scope
                .source_bindings
                .insert(input.input_hash.clone(), Arc::clone(&shared[&index]));
        }
        Ok(scope)
    }
    pub(super) fn owner_dependencies(
        &self,
    ) -> Result<Vec<(VaultRelativePath, Vec<ReadDependency>)>> {
        let mut entries = 0usize;
        let mut bytes = 0usize;
        let mut out = Vec::new();
        for owner in &self.owners {
            self.allowance.milliseconds()?;
            let path =
                owner.proof.documents.keys().next().ok_or_else(|| {
                    conflict("authenticated embedding owner document disappeared")
                })?;
            for (dependency, expected) in owner.proof.read_preconditions_iter() {
                entries = entries.saturating_add(1);
                bytes = bytes.saturating_add(guard_bytes(dependency, expected)?);
                if entries > MAX_BINDING_ENTRIES || bytes > MAX_BINDING_BYTES {
                    return Err(exhausted(
                        "owner dependency enrollment exceeds its allowance",
                    ));
                }
            }
            out.push((path.clone(), owner.read_preconditions()));
        }
        Ok(out)
    }
    fn append_owner(
        &mut self,
        catalog: &Catalog,
        path: &VaultRelativePath,
        settings: &EmbeddingSettings,
        units: &UnitBudget,
    ) -> Result<()> {
        if self.owners.len() >= MAX_OWNERS {
            return Err(exhausted("normalized embedding owner count exhausted"));
        }
        let remaining = self.allowance.budget()?;
        self.owners
            .try_reserve_exact(1)
            .map_err(|_| exhausted("normalized embedding owner allocation refused"))?;
        #[cfg(test)]
        let (cache_before, units_before) = (self.reader.usage(), units.usage());
        #[cfg(test)]
        let _cost_owner = crate::jobs::embedding_cost_probe::enter(
            crate::jobs::embedding_cost_probe::Stage::OwnerProof,
        );
        #[cfg(test)]
        crate::jobs::embedding_cost_probe::count(
            crate::jobs::embedding_cost_probe::Counter::OwnerCount,
            1,
        );
        let owner_result = indexed_units::materialize_owner(
            catalog,
            &self.reader,
            path,
            settings,
            &remaining,
            units,
        );
        #[cfg(test)]
        {
            let cache_after = self.reader.usage();
            let units_after = units.usage();
            attribution::cache(
                cache_after.rows.saturating_sub(cache_before.rows),
                cache_after.bytes.saturating_sub(cache_before.bytes),
            );
            crate::jobs::embedding_cost_probe::count(
                crate::jobs::embedding_cost_probe::Counter::OwnerUnitCount,
                units_after.units.saturating_sub(units_before.units),
            );
            crate::jobs::embedding_cost_probe::count(
                crate::jobs::embedding_cost_probe::Counter::OwnerRenderBytes,
                units_after
                    .render_bytes
                    .saturating_sub(units_before.render_bytes),
            );
            attribution::owner(
                units_after.units.saturating_sub(units_before.units),
                units_after
                    .render_bytes
                    .saturating_sub(units_before.render_bytes),
                owner_result
                    .as_ref()
                    .ok()
                    .map(AuthenticatedDocument::verification_work),
            );
        }
        let mut owner = owner_result?;
        // The helper has already completed its own final reread. Charge all of
        // that actual work immediately; never give another owner the old budget.
        self.allowance.debit(owner.verification_work())?;
        self.allowance.milliseconds()?;
        // Retained canonical states are charged once per owner, not once per
        // rendered input. The owner's existing proof is the sole guard storage.
        for (path, expected) in owner.proof.read_preconditions_iter() {
            self.allowance.milliseconds()?;
            self.inventory.reserve(1, guard_bytes(path, expected)?)?;
        }
        let index = self.owners.len();
        for unit in &owner.units {
            self.allowance.milliseconds()?;
            if self
                .input_owners
                .get(&unit.input_hash)
                .is_some_and(|owners| owners.last() == Some(&index))
            {
                continue;
            }
            if !self.input_owners.contains_key(&unit.input_hash) {
                let key_bytes = size_of::<(Blake3Hash, Vec<usize>)>()
                    .checked_add(unit.input_hash.as_str().len())
                    .ok_or_else(|| exhausted("normalized embedding input key byte overflow"))?;
                self.inventory.reserve(1, key_bytes)?;
                self.input_owners
                    .insert(unit.input_hash.clone(), Vec::new());
            }
            self.inventory.reserve(1, size_of::<usize>())?;
            let owners = self
                .input_owners
                .get_mut(&unit.input_hash)
                .expect("reserved input key");
            owners
                .try_reserve_exact(1)
                .map_err(|_| exhausted("normalized embedding owner link allocation refused"))?;
            owners.push(index);
        }
        self.units
            .try_reserve_exact(owner.units.len())
            .map_err(|_| exhausted("normalized embedding unit allocation refused"))?;
        // Move each UTF-8 input exactly once. The retained proof needs no duplicate
        // unit text for its byte/state recheck; its canonical rows remain bounded
        // by the same cumulative QuerySnapshot/proof allowances.
        self.units.append(&mut owner.units);
        owner.units = Vec::new();
        self.owners.push(owner);
        Ok(())
    }
    /// A single borrowed consistency pass rejects contradictory snapshots even
    /// across different input hashes, before any report or paid admission.
    fn verify_binding_states(&self) -> Result<()> {
        let mut inventory = self.inventory.clone();
        let mut states = BTreeMap::new();
        for owner in &self.owners {
            for (path, expected) in owner.proof.read_preconditions_iter() {
                self.allowance.milliseconds()?;
                retain_state(&mut states, &mut inventory, path, expected)?;
            }
        }
        Ok(())
    }
    /// Expand only the requested task's exact all-owner union. No corpus-wide
    /// expanded map is retained; all owner incidences must resolve fail closed.
    pub(super) fn batch_bindings(
        &self,
        hashes: &BTreeSet<&Blake3Hash>,
    ) -> Result<Vec<ReadDependency>> {
        self.allowance.milliseconds()?;
        if self.failed {
            return Err(conflict("cannot bind inputs from a failed owner proof"));
        }
        let mut inventory = BindingInventory::default();
        let mut states = BTreeMap::new();
        let mut seen_owners = BTreeSet::new();
        for hash in hashes {
            self.allowance.milliseconds()?;
            let indices = self
                .input_owners
                .get(*hash)
                .filter(|indices| !indices.is_empty())
                .ok_or_else(|| conflict("embedding batch input has no authenticated owner"))?;
            for index in indices {
                let owner = self
                    .owners
                    .get(*index)
                    .ok_or_else(|| conflict("embedding batch owner reference disappeared"))?;
                if seen_owners.contains(index) {
                    continue;
                }
                inventory.reserve(1, size_of::<usize>())?;
                seen_owners.insert(*index);
                for (path, expected) in owner.proof.read_preconditions_iter() {
                    self.allowance.milliseconds()?;
                    retain_state(&mut states, &mut inventory, path, expected)?;
                }
            }
        }
        // Reserve the complete output before cloning any guards. Charge both the
        // borrowed scratch entries and the owned task-boundary representation.
        for (path, expected) in &states {
            inventory.reserve(1, guard_bytes(path, expected)?)?;
        }
        let mut out = Vec::new();
        out.try_reserve_exact(states.len())
            .map_err(|_| exhausted("embedding batch guard allocation refused"))?;
        for (path, expected) in states {
            self.allowance.milliseconds()?;
            out.push(ReadDependency {
                path: path.clone(),
                expected: expected.clone(),
            });
        }
        Ok(out)
    }
    /// A failed batch cannot be retried with partially consumed proof allowances.
    /// Callers stop on any failure and never emit/commit the retained inputs.
    pub(super) fn recheck(&mut self, catalog: &Catalog) -> Result<()> {
        #[cfg(test)]
        let _cost_owner = crate::jobs::embedding_cost_probe::enter(
            crate::jobs::embedding_cost_probe::Stage::OwnerProof,
        );

        #[cfg(test)]
        let mut observed = attribution::Stage::new(true);
        if self.failed {
            return Err(conflict(
                "normalized embedding input proof previously failed",
            ));
        }
        self.failed = true;
        self.allowance.milliseconds()?;
        self.reader.verify_operations(catalog)?;
        for owner in &mut self.owners {
            #[cfg(test)]
            crate::jobs::embedding_cost_probe::count(
                crate::jobs::embedding_cost_probe::Counter::OwnerCount,
                1,
            );

            let remaining = self.allowance.budget()?;
            let before = owner.verification_work();
            let result = owner
                .proof
                .recheck_with_remaining(catalog, &self.reader, &remaining);
            let after = owner.verification_work();
            #[cfg(test)]
            attribution::proof((
                after.0.saturating_sub(before.0),
                after.1.saturating_sub(before.1),
                after.2.saturating_sub(before.2),
            ));
            // Also debit partial reads on failure. Meter work never decreases.
            self.allowance.debit((
                after.0.saturating_sub(before.0),
                after.1.saturating_sub(before.1),
                after.2.saturating_sub(before.2),
            ))?;
            result?;
        }
        self.allowance.milliseconds()?;
        self.reader.verify_operations(catalog)?;
        self.allowance.milliseconds()?;
        self.failed = false;
        #[cfg(test)]
        {
            observed.complete = true;
        }
        Ok(())
    }
}

pub(super) fn materialize(
    catalog: &Catalog,
    settings: &EmbeddingSettings,
    paths: Option<&[VaultRelativePath]>,
    budget: &VerificationBudget,
) -> Result<NormalizedEmbeddingInputs> {
    materialize_with_query_limits(catalog, settings, paths, budget, QueryReadLimits::default())
}
fn materialize_with_query_limits(
    catalog: &Catalog,
    settings: &EmbeddingSettings,
    paths: Option<&[VaultRelativePath]>,
    budget: &VerificationBudget,
    mut query_limits: QueryReadLimits,
) -> Result<NormalizedEmbeddingInputs> {
    #[cfg(test)]
    let mut observed = attribution::Stage::new(false);
    // This deadline starts before settings validation, path admission and reader acquisition.
    let allowance = Allowance::new(budget)?;
    settings.validate()?;
    let selected = match paths {
        None => None,
        Some(paths) => {
            if paths.len() > MAX_OWNERS {
                return Err(exhausted(
                    "normalized embedding selected owner count exhausted",
                ));
            }
            let mut bytes = 0usize;
            let mut selected: BTreeSet<&VaultRelativePath> = BTreeSet::new();
            for path in paths {
                allowance.milliseconds()?;
                if selected.contains(path) {
                    continue;
                }
                bytes = bytes
                    .checked_add(path.as_str().len())
                    .filter(|n| *n <= MAX_PATH_BYTES)
                    .ok_or_else(|| {
                        exhausted("normalized embedding selected path bytes exhausted")
                    })?;
                // Own only borrowed references during path admission.
                selected.insert(path);
            }
            Some(selected)
        }
    };
    query_limits.validate()?;
    query_limits.max_elapsed_ms = query_limits.max_elapsed_ms.min(allowance.milliseconds()?);
    let reader = catalog.cached_query_snapshot(query_limits)?;
    if !reader.normalized_layout() {
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "normalized embedding inputs require a normalized catalog",
        ));
    }
    let units = UnitBudget::with_deadline(UnitLimits::default(), allowance.deadline)?;
    let mut result = NormalizedEmbeddingInputs {
        snapshot: reader.snapshot().clone(),
        units: Vec::new(),
        input_owners: BTreeMap::new(),
        owners: Vec::new(),
        reader,
        allowance,
        inventory: BindingInventory::default(),
        failed: false,
    };
    if let Some(selected) = selected {
        for path in selected {
            result.append_owner(catalog, path, settings, &units)?;
        }
    } else {
        let mut after = None;
        loop {
            result.allowance.milliseconds()?;
            #[cfg(test)]
            let before = result.reader.usage();
            let page_result = result.reader.embedding_document_paths(after.as_ref(), 128);
            #[cfg(test)]
            {
                let after = result.reader.usage();
                attribution::cache(
                    after.rows.saturating_sub(before.rows),
                    after.bytes.saturating_sub(before.bytes),
                );
            }
            let page = page_result?;
            let last = page.len() < 128;
            after = page.last().cloned();
            for path in &page {
                result.append_owner(catalog, path, settings, &units)?;
            }
            if last {
                break;
            }
        }
    }
    result.verify_binding_states()?;
    result.reader.verify_operations(catalog)?;
    result.allowance.milliseconds()?;
    #[cfg(test)]
    {
        observed.complete = true;
    }
    Ok(result)
}

#[cfg(test)]
#[path = "../../tests/fixtures/p17/common.rs"]
mod workflow_common;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        sources::{CaptureRequest, ExtractionInput, SourceOrigin, SourcePlan, SourceStore},
        vault::{VaultFs, VaultRoot, WriterPermit},
    };
    use std::fs;

    struct Fixture {
        temp: tempfile::TempDir,
        catalog: Catalog,
        captures: Vec<(VaultRelativePath, RecordId)>,
    }
    fn path(s: &str) -> VaultRelativePath {
        VaultRelativePath::new(s).unwrap()
    }
    fn code<T>(r: Result<T>) -> ErrorCode {
        match r {
            Err(e) => e.code,
            Ok(_) => panic!("expected refusal"),
        }
    }
    fn apply(root: &std::path::Path, plan: SourcePlan) {
        if let Some(draft) = plan.draft {
            for op in draft.operations {
                let target = root.join(op.target.as_str());
                fs::create_dir_all(target.parent().unwrap()).unwrap();
                if let Some(bytes) = op.proposed {
                    fs::write(target, bytes).unwrap();
                } else {
                    fs::remove_file(target).unwrap();
                }
            }
        }
    }
    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            fs::write(temp.path().join("WIKI.md"), "---\nwiki_schema: '1'\nwiki_id: vault_inputs\nwiki_kind: vault\ntitle: Inputs\n---\n").unwrap();
            fs::write(temp.path().join("page.md"), "---\nwiki_schema: '1'\nwiki_id: page_inputs\nwiki_kind: page\ntitle: Page café 東京\nwiki_status: reviewed\nwiki_depends_on_ids: [assertion_inputs]\n---\nAuthored dependency evidence.\n").unwrap();
            fs::write(temp.path().join("entity.md"), "---\nwiki_schema: '1'\nwiki_id: entity_inputs\nwiki_kind: entity\ntitle: Entity\nwiki_status: active\nwiki_entity_type: component\n---\n").unwrap();
            fs::write(temp.path().join("run.md"), "---\nwiki_schema: '1'\nwiki_id: run_inputs\nwiki_kind: run\ntitle: Run\nwiki_status: planned\nwiki_created_at: '2026-10-06T00:00:00Z'\n---\n").unwrap();
            for p in ["plain-a.md", "plain-b.md"] {
                fs::write(
                    temp.path().join(p),
                    "# Shared café\n\nShared exact bytes.\n",
                )
                .unwrap();
            }
            let handle = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
            let mut captures = Vec::new();
            for _ in 0..2 {
                let plan = SourceStore::new(handle.clone())
                    .plan_capture(CaptureRequest {
                        title: "Same immutable capture".into(),
                        origin_kind: SourceOrigin::LocalFile,
                        origin: "fixture.txt".into(),
                        original: b"Captured exact evidence.\n".to_vec(),
                        extraction: ExtractionInput::Utf8Preserve,
                        media_type: None,
                    })
                    .unwrap();
                let content = plan
                    .draft
                    .as_ref()
                    .unwrap()
                    .operations
                    .iter()
                    .find(|op| op.target.as_str().ends_with("content.md"))
                    .unwrap()
                    .target
                    .clone();
                if captures.is_empty() {
                    // Authenticate Entity identity transitively through an
                    // accepted, exactly supported assertion, rather than making
                    // the Page depend on an unsupported Entity description.
                    fs::write(temp.path().join("support.md"), "---\nwiki_schema: '1'\nwiki_id: assertion_inputs\nwiki_kind: assertion\ntitle: Supported identity fixture\nwiki_status: accepted\nwiki_subject_id: entity_inputs\nwiki_object_id: entity_inputs\nwiki_predicate: uses\n---\nSupported fixture proposition.\n").unwrap();
                    let captured = b"Captured exact evidence.\n";
                    let mut evidence = format!("---\nwiki_schema: '1'\nwiki_id: evidence_inputs\nwiki_kind: evidence\ntitle: Exact fixture support\nwiki_status: active\nwiki_assertion_id: assertion_inputs\nwiki_source_id: '{}'\nwiki_source_revision: '{}'\nwiki_stance: supports\nwiki_locator_kind: utf8-bytes\nwiki_span_start: 0\nwiki_span_end: {}\nwiki_quote_hash: {}\n---\n", plan.source_id, plan.revision_id, captured.len(), Blake3Hash::digest(captured)).into_bytes();
                    evidence.extend(
                        crate::sources::evidence::exact_quote_body(captured, "\n", "Support")
                            .unwrap(),
                    );
                    fs::write(temp.path().join("evidence.md"), evidence).unwrap();
                }
                captures.push((content, plan.source_id.clone()));
                apply(temp.path(), plan);
            }
            let catalog = Catalog::new(handle, RecordId::new("vault_inputs").unwrap());
            let fixture = Self {
                temp,
                catalog,
                captures,
            };
            fixture.publish(true);
            fixture
        }
        fn publish(&self, rebuild: bool) {
            let writer =
                WriterPermit::acquire(self.catalog.fs().root(), Duration::from_secs(1)).unwrap();
            if rebuild {
                self.catalog.rebuild_normalized(&writer).unwrap();
            } else {
                self.catalog.sync_normalized(&writer).unwrap();
            }
        }
        fn materialize(&self, paths: Option<&[VaultRelativePath]>) -> NormalizedEmbeddingInputs {
            materialize(
                &self.catalog,
                &EmbeddingSettings::default(),
                paths,
                &VerificationBudget::default(),
            )
            .unwrap()
        }
    }
    fn hash_bindings(inputs: &NormalizedEmbeddingInputs, hash: &Blake3Hash) -> Vec<ReadDependency> {
        inputs.batch_bindings(&BTreeSet::from([hash])).unwrap()
    }
    fn all_bindings(inputs: &NormalizedEmbeddingInputs) -> Vec<ReadDependency> {
        inputs
            .batch_bindings(&inputs.input_owners.keys().collect())
            .unwrap()
    }
    #[test]
    fn selected_and_full_corpus_keep_exact_owners_and_real_published_snapshot() {
        let fixture = Fixture::new();
        let full = fixture.materialize(None);
        let selected = fixture.materialize(Some(&[path("page.md"), path("page.md")]));
        assert_eq!(full.owners.len(), 5);
        assert_eq!(selected.owners.len(), 1);
        assert_eq!(
            full.units
                .iter()
                .filter(|unit| unit.owner == path("page.md"))
                .collect::<Vec<_>>(),
            selected.units.iter().collect::<Vec<_>>()
        );
        assert!(
            full.units
                .iter()
                .all(|unit| unit.owner != path("run.md") && unit.owner != path("entity.md"))
        );
        assert_eq!(full.snapshot, *full.reader.snapshot());
        assert!(full.snapshot.publication().is_some());
        assert!(fixture.materialize(Some(&[])).units.is_empty());
    }
    #[test]
    fn matching_input_hashes_merge_captured_and_authored_selected_guards() {
        let fixture = Fixture::new();
        let full = fixture.materialize(None);
        let captured_units = full
            .units
            .iter()
            .filter(|unit| {
                unit.owner == fixture.captures[0].0 || unit.owner == fixture.captures[1].0
            })
            .collect::<Vec<_>>();
        assert_eq!(captured_units.len(), 2);
        assert_eq!(captured_units[0].input_hash, captured_units[1].input_hash);
        assert_ne!(captured_units[0].unit_id, captured_units[1].unit_id);
        let guards = hash_bindings(&full, &captured_units[0].input_hash);
        for (content, _) in &fixture.captures {
            let prefix = content.as_str().rsplit_once('/').unwrap().0;
            assert!(guards.iter().any(|guard| guard.path == *content));
            assert!(
                guards
                    .iter()
                    .any(|guard| guard.path.as_str() == format!("{prefix}/original.bin"))
            );
            assert!(
                guards
                    .iter()
                    .any(|guard| guard.path.as_str() == format!("{prefix}/revision.md"))
            );
        }
        for owner in full.owners.iter().filter(|owner| {
            owner
                .proof
                .documents
                .keys()
                .any(|p| fixture.captures.iter().any(|(content, _)| p == content))
        }) {
            for record in owner.proof.records.values().filter(|record| {
                matches!(
                    record.record.kind(),
                    RecordKind::Source | RecordKind::Revision
                )
            }) {
                assert!(guards.iter().any(|guard| guard.path == record.path));
            }
        }
        let page = full
            .units
            .iter()
            .find(|unit| unit.owner == path("page.md"))
            .unwrap();
        assert!(
            hash_bindings(&full, &page.input_hash)
                .iter()
                .any(|guard| guard.path == path("entity.md"))
        );
        let plain = full
            .units
            .iter()
            .find(|unit| unit.owner == path("plain-a.md"))
            .unwrap();
        let guards = hash_bindings(&full, &plain.input_hash);
        assert!(guards.iter().any(|guard| guard.path == path("plain-a.md")));
        assert!(guards.iter().any(|guard| guard.path == path("plain-b.md")));
        assert!(guards.windows(2).all(|pair| pair[0].path < pair[1].path));
        assert!(full.input_owners.len() < full.units.len());
        let proof_entries = full
            .owners
            .iter()
            .map(|owner| owner.proof.read_preconditions_iter().count())
            .sum::<usize>();
        let links = full.input_owners.values().map(Vec::len).sum::<usize>();
        assert_eq!(
            full.inventory.entries,
            full.input_owners.len() + links + proof_entries
        );
    }
    #[test]
    fn same_size_selected_dependency_and_captured_payload_edits_poison_recheck() {
        for target in ["dependency", "capture"] {
            let fixture = Fixture::new();
            let mut inputs = fixture.materialize(None);
            let (path, before, after) = if target == "dependency" {
                (path("entity.md"), "title: Entity", "title: Edited")
            } else {
                (
                    fixture.captures[0].0.clone(),
                    "Captured exact",
                    "Captured other",
                )
            };
            let raw = fs::read_to_string(fixture.temp.path().join(path.as_str())).unwrap();
            let changed = raw.replace(before, after);
            assert_eq!(raw.len(), changed.len());
            fs::write(fixture.temp.path().join(path.as_str()), changed).unwrap();
            assert_eq!(
                code(inputs.recheck(&fixture.catalog)),
                ErrorCode::FreshnessConflict
            );
            assert!(inputs.failed);
            assert_eq!(
                code(inputs.recheck(&fixture.catalog)),
                ErrorCode::FreshnessConflict
            );
        }
    }
    #[test]
    fn unrelated_job_publication_and_rebuild_preserve_exact_units_and_membership_guards() {
        let fixture = Fixture::new();
        let mut before = fixture.materialize(None);
        let raw = fs::read_to_string(fixture.temp.path().join("run.md")).unwrap();
        fs::write(
            fixture.temp.path().join("run.md"),
            raw.replace("title: Run", "title: Unrelated Run"),
        )
        .unwrap();
        fixture.publish(false);
        before.recheck(&fixture.catalog).unwrap();
        let after = fixture.materialize(None);
        assert_ne!(before.snapshot, after.snapshot);
        assert_eq!(before.units, after.units);
        assert_eq!(before.input_owners, after.input_owners);
        assert_eq!(all_bindings(&before), all_bindings(&after));
        fixture.publish(true);
        let rebuilt = fixture.materialize(None);
        assert_eq!(before.units, rebuilt.units);
        assert_eq!(before.input_owners, rebuilt.input_owners);
        assert_eq!(all_bindings(&before), all_bindings(&rebuilt));
    }
    #[test]
    fn multi_owner_final_recheck_stops_before_next_owner_when_shared_allowance_is_empty() {
        let fixture = Fixture::new();
        let mut inputs = materialize(
            &fixture.catalog,
            &EmbeddingSettings::default(),
            Some(&[path("plain-a.md"), path("plain-b.md")]),
            &VerificationBudget {
                max_files: 10,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(inputs.allowance.remaining.max_files, 2);
        crate::catalog::query_diagnostics::begin();
        let result = inputs.recheck(&fixture.catalog);
        let observation = crate::catalog::query_diagnostics::end();
        assert_eq!(code(result), ErrorCode::BudgetExceeded);
        assert_eq!(inputs.allowance.remaining.max_files, 0);
        let opens = observation
            .reads
            .iter()
            .filter(|read| read.layer == "proof-open")
            .collect::<Vec<_>>();
        assert_eq!(opens.len(), 2);
        assert!(opens.iter().all(|read| !read.path.ends_with("plain-b.md")));
        crate::catalog::query_diagnostics::begin();
        assert_eq!(
            code(inputs.recheck(&fixture.catalog)),
            ErrorCode::FreshnessConflict
        );
        assert!(crate::catalog::query_diagnostics::end().reads.is_empty());
    }
    #[test]
    fn missing_withdrawn_and_operational_selected_owners_refuse() {
        let fixture = Fixture::new();
        assert_eq!(
            code(materialize(
                &fixture.catalog,
                &EmbeddingSettings::default(),
                Some(&[path("absent.md")]),
                &VerificationBudget::default()
            )),
            ErrorCode::FreshnessConflict
        );
        assert_eq!(
            code(materialize(
                &fixture.catalog,
                &EmbeddingSettings::default(),
                Some(&[path("run.md")]),
                &VerificationBudget::default()
            )),
            ErrorCode::CapabilityUnavailable
        );
        apply(
            fixture.temp.path(),
            SourceStore::new(fixture.catalog.fs().clone())
                .plan_withdraw(&fixture.captures[0].1, "fixture withdrawal")
                .unwrap(),
        );
        fixture.publish(false);
        assert_eq!(
            code(materialize(
                &fixture.catalog,
                &EmbeddingSettings::default(),
                Some(&[fixture.captures[0].0.clone()]),
                &VerificationBudget::default()
            )),
            ErrorCode::CapabilityUnavailable
        );
        assert!(
            fixture
                .materialize(None)
                .units
                .iter()
                .all(|unit| unit.owner != fixture.captures[0].0)
        );
    }
    #[test]
    fn deadline_row_and_selected_path_caps_remain_finite() {
        let fixture = Fixture::new();
        let mut inputs = fixture.materialize(Some(&[path("plain-a.md")]));
        inputs.allowance.deadline = Instant::now();
        crate::catalog::query_diagnostics::begin();
        assert_eq!(
            code(inputs.recheck(&fixture.catalog)),
            ErrorCode::BudgetExceeded
        );
        assert!(crate::catalog::query_diagnostics::end().reads.is_empty());
        assert_eq!(
            code(materialize_with_query_limits(
                &fixture.catalog,
                &EmbeddingSettings::default(),
                None,
                &VerificationBudget::default(),
                QueryReadLimits {
                    max_rows: 1,
                    ..Default::default()
                }
            )),
            ErrorCode::BudgetExceeded
        );
        assert_eq!(
            code(materialize(
                &fixture.catalog,
                &EmbeddingSettings::default(),
                Some(&vec![path("plain-a.md"); MAX_OWNERS + 1]),
                &VerificationBudget::default()
            )),
            ErrorCode::BudgetExceeded
        );
    }
    #[test]
    fn compact_equal_input_owners_share_paid_guards_and_keep_all_acknowledgment_proofs() {
        let fixture = Fixture::new();
        let body = (0..3)
            .map(|index| {
                let mut paragraph = format!("Shared paragraph {index}: café 東京. ");
                paragraph.push_str(&"x".repeat(598 - paragraph.len()));
                paragraph.push_str("\n\n");
                paragraph
            })
            .collect::<String>();
        // An explicit shared heading fixes both document titles as well as the
        // exact payload; filename-derived titles would create distinct inputs.
        let body = format!("# Shared café\n\n{body}");
        for name in ["plain-a.md", "plain-b.md"] {
            fs::write(fixture.temp.path().join(name), &body).unwrap();
        }
        fixture.publish(false);
        let settings = EmbeddingSettings {
            max_input_bytes: 8000,
            quality_target_bytes: Some(1000),
            ..Default::default()
        };
        let mut inputs = materialize(
            &fixture.catalog,
            &settings,
            Some(&[path("plain-a.md"), path("plain-b.md")]),
            &VerificationBudget::default(),
        )
        .unwrap();
        assert_eq!(inputs.units.len(), 6);
        let missing = missing_inputs(&inputs.units);
        assert_eq!(missing.len(), 3);
        let paid = inputs.paid_scope(&inputs.units, &missing, 127).unwrap();
        assert_eq!(paid.units.len(), 3);
        assert!(
            paid.units
                .iter()
                .all(|unit| unit.owner == path("plain-a.md"))
        );
        let allocations: Vec<_> = paid.source_bindings.values().collect();
        assert!(
            allocations
                .windows(2)
                .all(|pair| Arc::ptr_eq(pair[0], pair[1]))
        );
        for hash in paid.source_bindings.keys() {
            let guards = hash_bindings(&inputs, hash);
            assert!(guards.iter().any(|guard| guard.path == path("plain-a.md")));
            assert!(guards.iter().any(|guard| guard.path == path("plain-b.md")));
            assert!(
                paid.source_bindings[hash]
                    .iter()
                    .all(|guard| guard.path != path("plain-b.md"))
            );
        }
        let dependencies = inputs.owner_dependencies().unwrap();
        assert_eq!(dependencies.len(), 2);
        for (owner, guards) in dependencies {
            assert!(guards.iter().any(|guard| guard.path == owner));
        }
        // Editing the non-supplier cannot change the frozen paid input identity,
        // but the complete retained acknowledgment proof must refuse the edit.
        fs::write(
            fixture.temp.path().join("plain-b.md"),
            body.replace("café", "cafe"),
        )
        .unwrap();
        assert_eq!(
            code(inputs.recheck(&fixture.catalog)),
            ErrorCode::FreshnessConflict
        );
        assert_eq!(
            code(inputs.paid_scope(&inputs.units, &missing, 127)),
            ErrorCode::FreshnessConflict
        );
        assert_eq!(
            code(inputs.batch_bindings(&inputs.input_owners.keys().collect())),
            ErrorCode::FreshnessConflict
        );
    }

    fn missing_inputs(units: &[RenderedUnit]) -> Vec<crate::providers::types::EmbeddingInput> {
        units
            .iter()
            .map(|unit| (unit.input_hash.clone(), unit.utf8.clone()))
            .collect::<BTreeMap<_, _>>()
            .into_iter()
            .map(|(input_hash, utf8)| crate::providers::types::EmbeddingInput { input_hash, utf8 })
            .collect()
    }

    #[test]
    fn lazy_batch_union_matches_exact_owner_oracle_and_missing_references_refuse() {
        let fixture = Fixture::new();
        let mut inputs = fixture.materialize(None);
        let wanted: BTreeSet<_> = inputs
            .units
            .iter()
            .filter(|unit| unit.owner == path("page.md") || unit.owner == fixture.captures[0].0)
            .map(|unit| &unit.input_hash)
            .collect();
        let expected: BTreeMap<_, _> = inputs
            .owner_dependencies()
            .unwrap()
            .into_iter()
            .filter(|(path, _)| {
                inputs
                    .units
                    .iter()
                    .any(|unit| &unit.owner == path && wanted.contains(&unit.input_hash))
            })
            .flat_map(|(_, guards)| guards.into_iter().map(|guard| (guard.path, guard.expected)))
            .collect();
        let actual = inputs.batch_bindings(&wanted).unwrap();
        assert_eq!(
            actual
                .iter()
                .map(|guard| (guard.path.clone(), guard.expected.clone()))
                .collect::<BTreeMap<_, _>>(),
            expected
        );
        assert!(actual.iter().all(|guard| guard.path != path("plain-a.md")));
        assert!(inputs.batch_bindings(&BTreeSet::new()).unwrap().is_empty());
        let missing = Blake3Hash::digest(b"absent input");
        assert_eq!(
            code(inputs.batch_bindings(&BTreeSet::from([&missing]))),
            ErrorCode::FreshnessConflict
        );
        let hash = inputs.units[0].input_hash.clone();
        inputs.input_owners.get_mut(&hash).unwrap().push(usize::MAX);
        assert_eq!(
            code(inputs.batch_bindings(&BTreeSet::from([&hash]))),
            ErrorCode::FreshnessConflict
        );
        inputs.input_owners.remove(&hash);
        assert_eq!(
            code(inputs.batch_bindings(&BTreeSet::from([&hash]))),
            ErrorCode::FreshnessConflict
        );
    }

    #[test]
    fn contradictory_expected_states_refuse_before_report_across_any_input_hashes() {
        // Preparation uses this exact pass without grouping by hashes: a path
        // mismatch is refused even when the contributing owners have disjoint inputs.
        let dependency = path("same.md");
        let before = ExpectedState::Hash(Blake3Hash::digest(b"before"));
        let after = ExpectedState::Hash(Blake3Hash::digest(b"after"));
        let mut states = BTreeMap::new();
        let mut inventory = BindingInventory::default();
        retain_state(&mut states, &mut inventory, &dependency, &before).unwrap();
        retain_state(&mut states, &mut inventory, &dependency, &before).unwrap();
        assert_eq!(inventory.entries, 1);
        assert_eq!(
            code(retain_state(
                &mut states,
                &mut inventory,
                &dependency,
                &after
            )),
            ErrorCode::FreshnessConflict
        );
        assert_eq!(inventory.entries, 1);
        // Challenge the actual consistency pass with independently authenticated
        // owner proofs from different snapshots. Both equal and disjoint input
        // hashes must fail before these inputs can be returned by preparation.
        for equal_hash in [true, false] {
            let left = Fixture::new();
            let right = Fixture::new();
            let mut before = left.materialize(Some(&[path("plain-a.md")]));
            if equal_hash {
                let wiki = fs::read_to_string(right.temp.path().join("WIKI.md")).unwrap();
                fs::write(
                    right.temp.path().join("WIKI.md"),
                    wiki.replace("title: Inputs", "title: Altered"),
                )
                .unwrap();
            } else {
                fs::write(
                    right.temp.path().join("plain-a.md"),
                    "# Different\n\nA different exact input.\n",
                )
                .unwrap();
            }
            right.publish(false);
            let mut after = right.materialize(Some(&[path("plain-a.md")]));
            assert_eq!(
                before.units[0].input_hash == after.units[0].input_hash,
                equal_hash
            );
            before.owners.push(after.owners.pop().unwrap());
            assert_eq!(
                code(before.verify_binding_states()),
                ErrorCode::FreshnessConflict
            );
        }
    }

    #[test]
    fn paid_supplier_selection_is_path_ordered_complete_and_truthfully_bounded() {
        let fixture = Fixture::new();
        let inputs = fixture.materialize(None);
        let units: Vec<_> = inputs
            .units
            .iter()
            .filter(|unit| fixture.captures.iter().any(|(path, _)| path == &unit.owner))
            .cloned()
            .collect();
        let missing = missing_inputs(&units);
        assert_eq!(missing.len(), 1);
        let dependencies = inputs.owner_dependencies().unwrap();
        let first = dependencies
            .iter()
            .find(|(path, _)| units.iter().any(|unit| &unit.owner == path))
            .unwrap();
        let fitting = inputs.paid_scope(&units, &missing, first.1.len()).unwrap();
        assert_eq!(
            fitting.source_bindings[&missing[0].input_hash].as_ref(),
            first.1
        );
        assert!(fitting.units.iter().all(|unit| unit.owner == first.0));
        let error = match inputs.paid_scope(&units, &missing, 0) {
            Err(error) => error,
            Ok(_) => panic!("expected oversized refusal"),
        };
        assert_eq!(error.code, ErrorCode::BudgetExceeded);
        let least = dependencies
            .iter()
            .filter(|(path, _)| units.iter().any(|unit| &unit.owner == path))
            .map(|(_, guards)| guards.len())
            .min()
            .unwrap();
        assert_eq!(error.details["required_source_guards"], least);
        assert_eq!(error.details["available_source_guards"], 0);
        assert_eq!(error.details["reserved_run_guards"], 1);
        let absent = crate::providers::types::EmbeddingInput {
            input_hash: Blake3Hash::digest(b"absent"),
            utf8: "absent".into(),
        };
        assert_eq!(
            code(inputs.paid_scope(&units, &[absent], 127)),
            ErrorCode::FreshnessConflict
        );
        let mut inventory = BindingInventory {
            entries: MAX_BINDING_ENTRIES,
            bytes: 0,
        };
        assert_eq!(code(inventory.reserve(1, 0)), ErrorCode::BudgetExceeded);
        assert_eq!(inventory.entries, MAX_BINDING_ENTRIES);
        let mut inventory = BindingInventory {
            entries: 0,
            bytes: MAX_BINDING_BYTES,
        };
        assert_eq!(code(inventory.reserve(0, 1)), ErrorCode::BudgetExceeded);
        assert_eq!(inventory.bytes, MAX_BINDING_BYTES);
    }

    /// Frozen synthetic mechanics fixture: 64 Sources, 175 distinct paragraphs
    /// per Source, exactly 600 UTF-8 bytes per paragraph. No hidden labels or
    /// external data; geometry is the ordinary 8000/1000 byte configuration.
    #[test]
    fn fine_chunk_factored_guards_complete_mock_sync_cached_citations_and_reuse() {
        use super::workflow_common as common;
        use crate::{
            app::{OfflineApp, OperationOptions, embeddings::EmbeddingRuntime},
            config::providers::ProviderConfig,
            jobs::*,
            providers::{credentials::*, dispatcher::Dispatcher, types::*},
            retrieval::{
                ContextRequest, ContextScope, ExcerptLabel, QueryPlan, SearchFilters, SearchLimits,
                SearchMode,
            },
            sources::CaptureAllocation,
        };
        use std::sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        };
        const SOURCE_COUNT: usize = 64;
        const PARAGRAPHS: usize = 175;
        const PARAGRAPH_BYTES: usize = 600;
        const REQUEST_LIMIT: usize = 512;
        const FACT: &str = "Guardneedle stores 47 cobalt tokens. Unicode café 東京 🦀.";
        let fixture = common::Fixture::new();
        let mut captures = Vec::new();
        let mut input_pin = blake3::Hasher::new();
        for source_index in 0..SOURCE_COUNT {
            let source = RecordId::new(format!("source_guard_fixture_{source_index:02}")).unwrap();
            let revision =
                RecordId::new(format!("revision_guard_fixture_{source_index:02}")).unwrap();
            let title = format!("Fine guard Source {source_index:02}");
            let mut body = String::new();
            for paragraph_index in 0..PARAGRAPHS {
                let mut paragraph = if source_index == 0 && paragraph_index == 0 {
                    format!("{FACT} Source {source_index:02} paragraph {paragraph_index:03}. ")
                } else {
                    format!(
                        "Fixture Source {source_index:02}, paragraph {paragraph_index:03}; deterministic local mechanics. "
                    )
                };
                paragraph.push_str(&"x".repeat(PARAGRAPH_BYTES - 2 - paragraph.len()));
                paragraph.push_str("\n\n");
                assert_eq!(paragraph.len(), PARAGRAPH_BYTES);
                body.push_str(&paragraph);
            }
            assert_eq!(body.len(), PARAGRAPHS * PARAGRAPH_BYTES);
            input_pin.update(title.as_bytes());
            input_pin.update(body.as_bytes());
            let plan = SourceStore::new(fixture.fs.clone())
                .plan_capture_named(
                    CaptureRequest {
                        title,
                        origin_kind: SourceOrigin::LocalFile,
                        origin: format!("fine-guard-{source_index:02}.txt"),
                        original: body.as_bytes().to_vec(),
                        extraction: ExtractionInput::Utf8Preserve,
                        media_type: None,
                    },
                    &CaptureAllocation {
                        source_id: source.clone(),
                        revision_id: revision.clone(),
                        captured_at: "2026-10-08T00:00:00Z".into(),
                    },
                )
                .unwrap();
            let content = plan
                .draft
                .as_ref()
                .unwrap()
                .operations
                .iter()
                .find(|operation| operation.target.as_str().ends_with("content.md"))
                .unwrap()
                .target
                .clone();
            apply(fixture.fs.root().path(), plan);
            captures.push((source, revision, content, body));
        }
        fixture.app.index_rebuild_normalized().unwrap();
        let settings = EmbeddingSettings {
            max_input_bytes: 8000,
            quality_target_bytes: Some(1000),
            ..Default::default()
        };
        let catalog = Catalog::new(fixture.fs.clone(), fixture.app.vault_id().clone());
        let wider_construction_budget = VerificationBudget {
            max_elapsed_ms: 30_000,
            ..Default::default()
        };
        let started = Instant::now();
        let mut prepared =
            materialize(&catalog, &settings, None, &wider_construction_budget).unwrap();
        let preparation_ns = started.elapsed().as_nanos();
        assert_eq!(prepared.owners.len(), SOURCE_COUNT);
        assert_eq!(prepared.units.len(), SOURCE_COUNT * PARAGRAPHS);
        assert_eq!(prepared.input_owners.len(), SOURCE_COUNT * PARAGRAPHS);
        assert!(
            prepared
                .input_owners
                .values()
                .all(|indices| indices.len() == 1)
        );
        // Independent exact production render/proof oracle, without the compact
        // associations or a reconstructed resident expanded production map.
        let oracle_reader = catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap();
        let oracle_budget = UnitBudget::new(UnitLimits::default()).unwrap();
        let mut paths: Vec<_> = captures
            .iter()
            .map(|(_, _, path, _)| path.clone())
            .collect();
        paths.sort();
        let mut oracle_units = Vec::new();
        let mut oracle_dependencies = Vec::new();
        let mut old_guard_entries = 0usize;
        let mut proof_entries = 0usize;
        let mut hashes = BTreeSet::new();
        for path in &paths {
            let owner = indexed_units::materialize_owner(
                &catalog,
                &oracle_reader,
                path,
                &settings,
                &wider_construction_budget,
                &oracle_budget,
            )
            .unwrap();
            assert_eq!(owner.units.len(), PARAGRAPHS);
            let owner_hashes: BTreeSet<_> = owner
                .units
                .iter()
                .map(|unit| unit.input_hash.clone())
                .collect();
            assert_eq!(owner_hashes.len(), PARAGRAPHS);
            assert!(owner_hashes.iter().all(|hash| hashes.insert(hash.clone())));
            let dependencies = owner.read_preconditions();
            assert!(dependencies.len() >= 5);
            old_guard_entries += owner_hashes.len() * dependencies.len();
            proof_entries += dependencies.len();
            oracle_units.extend(owner.units);
            oracle_dependencies.push((path.clone(), dependencies));
        }
        let old_expanded_entries = hashes.len() + old_guard_entries;
        assert!(
            old_expanded_entries > MAX_BINDING_ENTRIES,
            "old expanded cardinality {old_expanded_entries}"
        );
        assert_eq!(prepared.units, oracle_units);
        assert_eq!(prepared.owner_dependencies().unwrap(), oracle_dependencies);
        let links = prepared.input_owners.values().map(Vec::len).sum::<usize>();
        assert_eq!(links, SOURCE_COUNT * PARAGRAPHS);
        assert_eq!(
            prepared.inventory.entries,
            proof_entries + hashes.len() + links
        );
        assert!(prepared.inventory.entries < MAX_BINDING_ENTRIES);
        assert!(prepared.inventory.bytes < MAX_BINDING_BYTES);
        prepared.recheck(&catalog).unwrap();
        let missing = missing_inputs(&prepared.units);
        let suppliers = prepared.paid_scope(&prepared.units, &missing, 127).unwrap();
        let mut paid_units = suppliers.units.clone();
        paid_units.sort_by(|left, right| left.unit_id.cmp(&right.unit_id));
        let mut exact_units = prepared.units.clone();
        exact_units.sort_by(|left, right| left.unit_id.cmp(&right.unit_id));
        assert_eq!(paid_units, exact_units);
        drop(paid_units);
        drop(exact_units);
        for indices in prepared.input_owners.values() {
            assert_eq!(indices.len(), 1);
        }
        for owner in &paths {
            let guards: Vec<_> = prepared
                .units
                .iter()
                .filter(|unit| &unit.owner == owner)
                .map(|unit| &suppliers.source_bindings[&unit.input_hash])
                .collect();
            assert_eq!(guards.len(), PARAGRAPHS);
            assert!(guards.windows(2).all(|pair| Arc::ptr_eq(pair[0], pair[1])));
        }
        let exact_supplier_bindings = suppliers.source_bindings;
        eprintln!(
            "fine-guard-fixture {}",
            serde_json::json!({
                "payload_pin":input_pin.finalize().to_hex().as_str(), "source_count":SOURCE_COUNT,
                "payload_bytes":SOURCE_COUNT*PARAGRAPHS*PARAGRAPH_BYTES,
                "units":prepared.units.len(), "input_hashes":hashes.len(),
                "old_expanded_entries":old_expanded_entries,"retained_proof_guards":proof_entries,
                "retained_owner_links":links,"actual_inventory_entries":prepared.inventory.entries,
                "actual_inventory_bytes":prepared.inventory.bytes,"entry_limit":MAX_BINDING_ENTRIES,
                "byte_limit":MAX_BINDING_BYTES,"materialize_elapsed_ns":preparation_ns,
                "request_limit":REQUEST_LIMIT,"response_accounting_limit_bytes":REQUEST_LIMIT as u64*8*1024*1024,
                "process_tree_peak_bytes":"unavailable","physical_io_bytes":"unavailable"
            })
        );
        drop(prepared);
        drop(oracle_reader);

        if let Some(outputs) = std::env::var_os("TEST_UNDECLARED_OUTPUTS_DIR") {
            fn retain_fixture(from: &std::path::Path, to: &std::path::Path, total: &mut u64) {
                // A disposable idle fixture only; refuse overwrite and symlinks.
                fs::create_dir(to).unwrap();
                for entry in fs::read_dir(from).unwrap() {
                    let entry = entry.unwrap();
                    let kind = entry.file_type().unwrap();
                    assert!(!kind.is_symlink());
                    let source = entry.path();
                    let target = to.join(entry.file_name());
                    if kind.is_dir() {
                        retain_fixture(&source, &target, total);
                    } else {
                        assert!(kind.is_file());
                        if entry.file_name().to_string_lossy().ends_with("-shm") {
                            continue;
                        }
                        *total = total.checked_add(entry.metadata().unwrap().len()).unwrap();
                        assert!(*total <= 128 * 1024 * 1024);
                        fs::copy(&source, &target).unwrap();
                    }
                }
            }
            let target = std::path::PathBuf::from(outputs).join("fine-guard-pristine-001");
            let mut bytes = 0;
            retain_fixture(fixture.fs.root().path(), &target, &mut bytes);
            eprintln!(
                "fine-guard-pristine {}",
                serde_json::json!({"root":target,"copied_bytes":bytes,
                "vault_id":fixture.app.vault_id(),"providers_copied":false})
            );
        }

        // The mock reads each run's immutable task inventory once, and returns
        // one deterministic vector per real task input. It never sees labels.
        struct BatchMock {
            fs: VaultFs,
            vault: RecordId,
            calls: AtomicUsize,
            items: AtomicUsize,
            references: Mutex<BTreeMap<RecordId, BTreeMap<Blake3Hash, BoundedPayloadRef>>>,
            supplied: InputBindings,
        }
        impl Transport for BatchMock {
            fn execute<'a>(
                &'a self,
                request: AuthenticatedRequest<'a>,
                _: TransportContext,
            ) -> TransportFuture<'a> {
                assert!(self.calls.fetch_add(1, Ordering::SeqCst) < REQUEST_LIMIT);
                let summary = request.summary();
                let mut references = self.references.lock().unwrap();
                if !references.contains_key(&summary.attempt.run_id) {
                    let job = JobLedger::new(
                        self.fs.clone(),
                        self.vault.clone(),
                        summary.attempt.run_id.clone(),
                        common::options(),
                    )
                    .unwrap();
                    let inspection = job.inspect().unwrap();
                    let mut frozen = BTreeMap::new();
                    for (key, task) in inspection.tasks {
                        assert!(task.spec.source_bindings.len() <= 127);
                        frozen.insert(key, task.spec.input);
                    }
                    references.insert(summary.attempt.run_id.clone(), frozen);
                }
                let reference =
                    references[&summary.attempt.run_id][&summary.attempt.task_key].clone();
                drop(references);
                let bytes = fs::read(self.fs.root().path().join(reference.path.as_str())).unwrap();
                assert_eq!(bytes.len() as u64, reference.byte_len);
                assert!(bytes.len() <= 262144);
                assert_eq!(Blake3Hash::digest(&bytes), reference.hash);
                let remote: RemoteInput = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(
                    crate::graph::packet::canonical_json(&remote).unwrap(),
                    bytes
                );
                let RemoteOperation::Embed { inputs, .. } = remote.operation else {
                    panic!("non-embedding task");
                };
                assert!(!inputs.is_empty() && inputs.len() <= 32);
                self.items.fetch_add(inputs.len(), Ordering::SeqCst);
                let data: Vec<_> = inputs
                    .iter()
                    .enumerate()
                    .map(|(index, input)| {
                        assert_eq!(input.input_hash, Blake3Hash::digest(input.utf8.as_bytes()));
                        if input.utf8 != "Guardneedle" {
                            assert!(self.supplied.contains_key(&input.input_hash));
                        }
                        let vector = if input.utf8.contains(FACT) || input.utf8 == "Guardneedle" {
                            vec![1.0, 0.0]
                        } else {
                            vec![0.0, 1.0]
                        };
                        serde_json::json!({"index":index,"embedding":vector})
                    })
                    .collect();
                let response = TransportReply::new(
                    200,
                    vec![],
                    serde_json::to_vec(&serde_json::json!({
                        "model":"test-model","data":data,
                        "usage":{"prompt_tokens":inputs.len()*2,"total_tokens":inputs.len()*2}
                    }))
                    .unwrap(),
                )
                .unwrap();
                Box::pin(async move { Ok(response) })
            }
        }
        struct NoHelpers(Arc<AtomicUsize>);
        impl HelperRunner for NoHelpers {
            fn run(
                &self,
                _: &HelperInvocation,
                _: &HelperLimits,
                _: &dyn JobClock,
                _: &CancellationToken,
            ) -> Result<HelperOutput> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Err(WikiError::invalid(
                    "fine guard mock forbids credential helpers",
                ))
            }
        }
        let config = fs::read_to_string(&fixture.config)
            .unwrap()
            .replace("max_batch_items=1", "max_batch_items=64");
        common::private_write(&fixture.config, config);
        let service = ProviderConfig::load(&fixture.config)
            .unwrap()
            .authorize(
                &fixture.fs,
                fixture.app.vault_id(),
                "primary",
                Capability::Embed,
            )
            .unwrap();
        let transport = Arc::new(BatchMock {
            fs: fixture.fs.clone(),
            vault: fixture.app.vault_id().clone(),
            calls: AtomicUsize::new(0),
            items: AtomicUsize::new(0),
            references: Mutex::new(BTreeMap::new()),
            supplied: exact_supplier_bindings,
        });
        let helpers = Arc::new(AtomicUsize::new(0));
        let dispatcher = Dispatcher::new(
            fixture.fs.clone(),
            DispatchOptions {
                broker: Arc::new(CredentialBroker::new(CredentialOptions {
                    clock: Arc::new(common::TestClock),
                    inputs: Arc::new(NativeSecretInputs),
                    runner: Arc::new(NoHelpers(helpers.clone())),
                })),
                transport: transport.clone(),
                jitter: Arc::new(FineZeroJitter),
            },
        );
        struct FineZeroJitter;
        impl JitterSource for FineZeroJitter {
            fn sample_inclusive(&self, _: u64) -> Result<u64> {
                Ok(0)
            }
        }
        let runtime = EmbeddingRuntime::new(
            &service,
            &dispatcher,
            common::options(),
            LifetimeLimits {
                requests: REQUEST_LIMIT as u64,
                concurrency: 1,
                attempts_per_task: 1,
                request_bytes: Some(64 * 1024 * 1024),
                // Unknown response/cost holds remain accounted; this finite ceiling
                // admits 512 full 8-MiB reservations, not 4 GiB of actual replies.
                response_bytes: Some(REQUEST_LIMIT as u64 * 8 * 1024 * 1024),
                ..Default::default()
            },
            1_800_000_000_000,
            1_800_000_120_000,
            None,
        );
        let check_start = Instant::now();
        let check = fixture
            .app
            .embeddings_check(&settings, Some(&runtime), false)
            .unwrap();
        let check_ns = check_start.elapsed().as_nanos();
        assert_eq!(check.coverage.eligible_units, hashes.len());
        assert_eq!(check.coverage.missing_units, hashes.len());
        assert!(!check.network_used);
        assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
        assert_eq!(helpers.load(Ordering::SeqCst), 0);
        fn durable_files(root: &std::path::Path) -> BTreeMap<std::path::PathBuf, Blake3Hash> {
            fn collect(
                root: &std::path::Path,
                path: &std::path::Path,
                out: &mut BTreeMap<std::path::PathBuf, Blake3Hash>,
            ) {
                for entry in fs::read_dir(path).unwrap() {
                    let entry = entry.unwrap();
                    let path = entry.path();
                    if entry.file_type().unwrap().is_dir() {
                        collect(root, &path, out);
                    } else {
                        // SQLite shared-memory lock bytes are volatile reader
                        // state. All durable files and path additions are checked.
                        if path
                            .file_name()
                            .unwrap()
                            .to_string_lossy()
                            .ends_with("-shm")
                        {
                            continue;
                        }
                        out.insert(
                            path.strip_prefix(root).unwrap().to_path_buf(),
                            Blake3Hash::digest(fs::read(path).unwrap()),
                        );
                    }
                }
            }
            let mut out = BTreeMap::new();
            collect(root, root, &mut out);
            out
        }
        let before_dry_run = durable_files(fixture.fs.root().path());
        let dry_run = OfflineApp::new(
            fixture.fs.clone(),
            OperationOptions {
                offline: true,
                dry_run: true,
                ..Default::default()
            },
        )
        .unwrap();
        let mut dry_runtime = runtime.clone();
        dry_runtime.job_options.policy.dry_run = true;
        dry_runtime.job_options.policy.offline = true;
        let dry_report = dry_run
            .embeddings_check(&settings, Some(&dry_runtime), false)
            .unwrap();
        assert_eq!(dry_report.coverage.eligible_units, hashes.len());
        assert!(!dry_report.network_used);
        assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
        assert_eq!(durable_files(fixture.fs.root().path()), before_dry_run);
        assert_eq!(helpers.load(Ordering::SeqCst), 0);
        let sync_start = Instant::now();
        let (sync_result, sync_work) =
            attribution::with_observation(|| fixture.app.embeddings_sync(&settings, &runtime));
        let synced = sync_result.unwrap();
        let sync_ns = sync_start.elapsed().as_nanos();
        assert!(synced.published);
        assert!(synced.network_used);
        assert_eq!(synced.generated_inputs, hashes.len());
        assert_eq!(synced.reused_inputs, 0);
        assert_eq!(synced.coverage.eligible_units, hashes.len());
        assert_eq!(synced.coverage.available_units, hashes.len());
        assert_eq!(synced.coverage.missing_units, 0);
        assert_eq!(synced.coverage.corrupt_units, 0);
        assert_eq!(synced.coverage.pending_units, 0);
        assert!(sync_ns <= Duration::from_secs(120).as_nanos());
        assert_eq!(transport.items.load(Ordering::SeqCst), hashes.len());
        // Discover the complete partitioned document workflow before the query
        // acquisition can create a separate embeddings_query accounting job.
        let document_runs = crate::vault::operational::RunStore::discover_existing(
            &fixture.fs,
            fixture.app.vault_id(),
            4096,
            &mut || Ok(()),
        )
        .unwrap();
        assert!(
            document_runs.len() > 1,
            "fine fixture must exercise multiple bounded jobs"
        );
        assert!(document_runs.contains(synced.run_id.as_ref().unwrap()));
        assert_eq!(
            sync_work.prior_accounting_discovery_calls,
            document_runs.len()
        );
        let mut marker_bytes = BTreeMap::new();
        for entry in
            fs::read_dir(fixture.fs.root().path().join(".wiki/state/embedding-jobs")).unwrap()
        {
            let entry = entry.unwrap();
            assert!(entry.file_type().unwrap().is_file());
            let bytes = fs::read(entry.path()).unwrap();
            let marker: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            let run: RecordId = serde_json::from_value(marker["run_id"].clone()).unwrap();
            assert!(marker_bytes.insert(run, bytes.len()).is_none());
        }
        let mut document_encoding_sizes = Vec::new();
        let mut document_task_count = 0usize;
        let mut document_hashes = BTreeSet::new();
        let mut document_requests = 0u64;
        let mut document_budgets = Vec::new();
        for run in &document_runs {
            let job = JobLedger::new(
                fixture.fs.clone(),
                fixture.app.vault_id().clone(),
                run.clone(),
                common::options(),
            )
            .unwrap();
            let inspection = job.inspect().unwrap();
            assert_eq!(inspection.spec.scope.operation, "embeddings_sync");
            assert_eq!(inspection.state, RunState::Completed);
            assert_eq!(inspection.spec.created_at_utc_ms, runtime.created_at_utc_ms);
            assert_eq!(inspection.spec.deadline_utc_ms, runtime.deadline_utc_ms);
            assert!(
                inspection
                    .tasks
                    .values()
                    .all(|task| task.state == TaskState::Completed)
            );
            JobLedger::validate_bootstrap_size(&inspection.spec).unwrap();
            let spec_bytes = serde_json::to_vec(&inspection.spec).unwrap().len();
            use std::io::Read;
            let journal_path = fixture
                .fs
                .root()
                .path()
                .join(format!(".wiki/state/jobs/{run}/journal.bin"));
            let mut journal = fs::File::open(journal_path).unwrap();
            let mut header = [0u8; 44];
            journal.read_exact(&mut header).unwrap();
            assert_eq!(&header[..8], b"LWJOB001");
            assert_eq!(blake3::hash(&header[..12]).as_bytes(), &header[12..44]);
            let genesis_body_bytes = u32::from_le_bytes(header[8..12].try_into().unwrap()) as usize;
            assert!(genesis_body_bytes > 0 && genesis_body_bytes <= EVENT_MAX_BYTES - 76);
            let mut genesis_body = vec![0u8; genesis_body_bytes];
            journal.read_exact(&mut genesis_body).unwrap();
            let mut checksum = [0u8; 32];
            journal.read_exact(&mut checksum).unwrap();
            assert_eq!(blake3::hash(&genesis_body).as_bytes(), &checksum);
            let genesis: LedgerEvent = serde_json::from_slice(&genesis_body).unwrap();
            let EventPayload::Genesis {
                spec: genesis_spec, ..
            } = &genesis.payload
            else {
                panic!("first frame must be Genesis");
            };
            assert_eq!(genesis_spec, &inspection.spec);
            let (_, encoded) = crate::jobs::events::encode(&genesis).unwrap();
            assert_eq!(encoded.len(), genesis_body_bytes + 76);
            let run_note = inspection.run_note.as_ref().unwrap();
            let actual_run_bytes =
                fs::read(fixture.fs.root().path().join(run_note.path.as_str())).unwrap();
            assert_eq!(Blake3Hash::digest(&actual_run_bytes), run_note.hash);
            document_encoding_sizes.push(
                serde_json::json!({"run_id":run,"spec_json_bytes":spec_bytes,
                "genesis_body_bytes":genesis_body_bytes,"genesis_frame_bytes":encoded.len(),
                "current_run_bytes":actual_run_bytes.len(),"marker_bytes":marker_bytes[run]}),
            );
            document_task_count += inspection.tasks.len();
            document_requests += inspection.budget.dispatched_requests;
            // Preserve actual unknown-cost/response holds; never equate a
            // completed output with zero outstanding accounting obligations.
            document_budgets.push(serde_json::json!({"run_id":run,"budget":inspection.budget}));
            for task in inspection.tasks.values() {
                let reference = &task.spec.input;
                let bytes =
                    fs::read(fixture.fs.root().path().join(reference.path.as_str())).unwrap();
                assert_eq!(bytes.len() as u64, reference.byte_len);
                assert_eq!(Blake3Hash::digest(&bytes), reference.hash);
                let remote: RemoteInput = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(
                    crate::graph::packet::canonical_json(&remote).unwrap(),
                    bytes
                );
                let RemoteOperation::Embed { inputs, .. } = remote.operation else {
                    panic!("non-embedding task");
                };
                for input in &inputs {
                    assert_eq!(Blake3Hash::digest(input.utf8.as_bytes()), input.input_hash);
                    assert!(
                        document_hashes.insert(input.input_hash.clone()),
                        "partition repeated a paid input"
                    );
                }
                let expected: BTreeMap<_, _> = inputs
                    .iter()
                    .flat_map(|input| transport.supplied[&input.input_hash].iter())
                    .map(|guard| (guard.path.clone(), guard.expected.clone()))
                    .collect();
                let actual: BTreeMap<_, _> = task
                    .spec
                    .source_bindings
                    .iter()
                    .map(|guard| (guard.path.clone(), guard.expected.clone()))
                    .collect();
                assert_eq!(actual, expected);
                assert!(actual.len() <= 127);
            }
        }
        assert_eq!(document_hashes, hashes);
        assert_eq!(
            document_requests as usize,
            transport.calls.load(Ordering::SeqCst)
        );
        assert_eq!(document_task_count, transport.calls.load(Ordering::SeqCst));
        assert!(document_requests <= REQUEST_LIMIT as u64);
        let checked = fixture
            .app
            .embeddings_check(&settings, None, false)
            .unwrap();
        assert_eq!(checked.coverage.eligible_units, hashes.len());
        assert_eq!(checked.coverage.available_units, hashes.len());
        assert_eq!(checked.coverage.missing_units, 0);
        let query_plan = |mode| QueryPlan {
            mode,
            filters: SearchFilters {
                source_ids: vec![captures[0].0.clone()],
                ..Default::default()
            },
            limits: SearchLimits {
                hits: 1,
                candidates: 8,
                excerpt_bytes: 256,
            },
            cursor: None,
        };
        let before_query = transport.calls.load(Ordering::SeqCst);
        let acquired = fixture
            .app
            .semantic_search_selected(
                "Guardneedle",
                &query_plan(SearchMode::Semantic),
                Some(&runtime),
                false,
            )
            .unwrap();
        assert!(acquired.network_used);
        assert_eq!(acquired.hits.len(), 1);
        assert_eq!(transport.calls.load(Ordering::SeqCst), before_query + 1);
        fs::remove_file(&fixture.config).unwrap();
        let offline = fixture.offline();
        for mode in [SearchMode::Semantic, SearchMode::Hybrid] {
            let hits = offline
                .semantic_search_selected("Guardneedle", &query_plan(mode), None, false)
                .unwrap();
            assert!(!hits.network_used);
            assert_eq!(hits.hits.len(), 1);
            assert_eq!(hits.hits[0].source_id.as_ref(), Some(&captures[0].0));
            assert_eq!(hits.hits[0].owner_revision.as_ref(), Some(&captures[0].1));
            assert_eq!(hits.hits[0].locator.path, captures[0].2);
            assert!(hits.hits[0].excerpt.text.contains(FACT));
            for excerpt in
                std::iter::once(&hits.hits[0].excerpt).chain(&hits.hits[0].secondary_excerpts)
            {
                assert_eq!(excerpt.label, ExcerptLabel::CapturedSource);
                let slice = captures[0]
                    .3
                    .get(excerpt.span.start() as usize..excerpt.span.end() as usize)
                    .unwrap();
                assert_eq!(slice, excerpt.text);
                let Some(CitationRef::Source(citation)) = &excerpt.citation else {
                    panic!("Source citation required");
                };
                assert_eq!(citation.source_id, captures[0].0);
                assert_eq!(citation.source_revision, captures[0].1);
                assert_eq!(citation.span, excerpt.span);
                assert_eq!(citation.quote_hash, Blake3Hash::digest(slice.as_bytes()));
            }
            let context = offline
                .semantic_context(
                    "Guardneedle",
                    &ContextRequest {
                        scope: ContextScope::IndexedDocuments,
                        documents: query_plan(mode),
                        ..Default::default()
                    },
                    None,
                    true,
                    false,
                )
                .unwrap();
            assert!(!context.network_used);
            assert!(context.text().contains(FACT));
            assert!(!context.passages().is_empty());
            for passage in context.passages() {
                assert_eq!(passage.locator.path, captures[0].2);
                let slice = captures[0]
                    .3
                    .get(passage.span.start() as usize..passage.span.end() as usize)
                    .unwrap();
                assert_eq!(slice, passage.text);
                assert!(!passage.citations.is_empty());
                for citation in &passage.citations {
                    let CitationRef::Source(citation) = citation else {
                        panic!("Source citation required");
                    };
                    assert_eq!(citation.source_id, captures[0].0);
                    assert_eq!(citation.source_revision, captures[0].1);
                    assert_eq!(citation.span, passage.span);
                    assert_eq!(citation.quote_hash, Blake3Hash::digest(slice.as_bytes()));
                }
            }
        }
        let calls = transport.calls.load(Ordering::SeqCst);
        let reused = offline.embeddings_sync_cached(&settings).unwrap();
        assert!(reused.published);
        assert_eq!(reused.generated_inputs, 0);
        assert_eq!(reused.reused_inputs, 0);
        assert_eq!(reused.coverage.eligible_units, hashes.len());
        assert_eq!(reused.coverage.available_units, hashes.len());
        assert_eq!(reused.coverage.missing_units, 0);
        assert!(!reused.network_used);
        assert_eq!(transport.calls.load(Ordering::SeqCst), calls);
        assert_eq!(helpers.load(Ordering::SeqCst), 0);
        eprintln!(
            "fine-guard-workflow {}",
            serde_json::json!({
                "check_elapsed_ns":check_ns,"mock_sync_elapsed_ns":sync_ns,
                "document_requests":before_query,"all_mock_requests":calls,
                "generated_inputs":synced.generated_inputs,"eligible_units":synced.coverage.eligible_units,
                "tasks":document_task_count,"document_runs":document_runs.len(),
                "document_job_budgets":document_budgets,"document_job_encoding_sizes":document_encoding_sizes,
                "prior_accounting_discoveries":sync_work.prior_accounting_discovery_calls,"network_is_mock":true,"credential_helpers":helpers.load(Ordering::SeqCst),
                "returned_citation_modes":["semantic","hybrid"],"offline_reuse_provider_calls":0
            })
        );
    }
    #[test]
    #[ignore = "frozen paired Run-cost diagnostic; root supplies owned paths and shared config"]
    fn embedding_cost_exact_prefix_diagnostic() {
        embedding_cost_diagnostic(crate::jobs::embedding_cost_probe::Arm::Exact);
    }
    #[test]
    #[ignore = "frozen paired Run-cost diagnostic; root supplies owned paths and shared config"]
    fn embedding_cost_guard_occurrences_127_diagnostic() {
        embedding_cost_diagnostic(crate::jobs::embedding_cost_probe::Arm::GuardOccurrences127);
    }
    fn embedding_cost_diagnostic(arm: crate::jobs::embedding_cost_probe::Arm) {
        use super::workflow_common as common;
        use crate::jobs::embedding_cost_probe::{self as probe, Arm};
        use crate::{
            app::{OfflineApp, OperationOptions, embeddings::EmbeddingRuntime},
            config::providers::ProviderConfig,
            jobs::*,
            providers::{credentials::*, dispatcher::Dispatcher, types::*},
            retrieval::{spaces::SpaceSpec, vectors::VectorStore},
        };
        use std::{
            path::{Path, PathBuf},
            sync::{
                Mutex,
                atomic::{AtomicUsize, Ordering},
            },
        };
        let env_path = |name| PathBuf::from(std::env::var_os(name).expect(name));
        let pristine = env_path("LWIKI_EMBEDDING_COST_PRISTINE");
        let destination = env_path("LWIKI_EMBEDDING_COST_VAULT");
        let config = env_path("LWIKI_EMBEDDING_COST_CONFIG");
        let protocol_path = env_path("LWIKI_EMBEDDING_COST_PROTOCOL");
        let output = env_path("LWIKI_EMBEDDING_COST_OUTPUT");
        // The same owning interval includes copy, opening and protocol preparation.
        let preflight_started = Instant::now();
        assert!(output.is_dir());
        fn files(root: &Path) -> BTreeMap<String, (u64, Blake3Hash)> {
            fn collect(root: &Path, at: &Path, out: &mut BTreeMap<String, (u64, Blake3Hash)>) {
                for entry in fs::read_dir(at).unwrap() {
                    let entry = entry.unwrap();
                    let kind = entry.file_type().unwrap();
                    assert!(!kind.is_symlink());
                    let path = entry.path();
                    if kind.is_dir() {
                        collect(root, &path, out);
                    } else {
                        assert!(kind.is_file());
                        if entry.file_name().to_string_lossy().ends_with("-shm") {
                            continue;
                        }
                        let length = entry.metadata().unwrap().len();
                        assert!(length <= 128 * 1024 * 1024);
                        out.insert(
                            path.strip_prefix(root).unwrap().to_str().unwrap().into(),
                            (length, Blake3Hash::digest(fs::read(path).unwrap())),
                        );
                    }
                }
            }
            let mut out = BTreeMap::new();
            collect(root, root, &mut out);
            out
        }
        fn copy(from: &Path, to: &Path, total: &mut u64) {
            fs::create_dir(to).unwrap(); // Never overwrite a retained or prior arm.
            for entry in fs::read_dir(from).unwrap() {
                let entry = entry.unwrap();
                let kind = entry.file_type().unwrap();
                assert!(!kind.is_symlink());
                if kind.is_dir() {
                    copy(&entry.path(), &to.join(entry.file_name()), total);
                } else {
                    assert!(kind.is_file());
                    if entry.file_name().to_string_lossy().ends_with("-shm") {
                        continue;
                    }
                    *total = total.checked_add(entry.metadata().unwrap().len()).unwrap();
                    assert!(*total <= 128 * 1024 * 1024);
                    fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
                }
            }
        }
        fn write_json(path: &Path, value: &serde_json::Value) -> usize {
            use std::io::Write;
            let bytes = serde_json::to_vec(value).unwrap();
            assert!(bytes.len() <= 2 * 1024 * 1024);
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .unwrap();
            file.write_all(&bytes).unwrap();
            file.sync_all().unwrap();
            bytes.len()
        }
        let pristine_files = files(&pristine);
        assert!(pristine_files.contains_key("WIKI.md"));
        let mut copied_bytes = 0;
        copy(&pristine, &destination, &mut copied_bytes);
        assert_eq!(files(&destination), pristine_files);
        let fs_handle = VaultFs::new(VaultRoot::explicit(&destination).unwrap());
        let app = OfflineApp::new(fs_handle.clone(), OperationOptions::default()).unwrap();
        let service = ProviderConfig::load(&config)
            .unwrap()
            .authorize(&fs_handle, app.vault_id(), "primary", Capability::Embed)
            .unwrap();
        let settings = EmbeddingSettings {
            max_input_bytes: 8000,
            quality_target_bytes: Some(1000),
            ..Default::default()
        };
        let spec = SpaceSpec::from_service(&service, settings.clone()).unwrap();
        assert_eq!(service.summary().model.as_deref(), Some("test-model"));
        assert_eq!(spec.dimensions, Some(2));
        let catalog = Catalog::new(fs_handle.clone(), app.vault_id().clone());
        let mut prepared = materialize(
            &catalog,
            &settings,
            None,
            &VerificationBudget {
                max_elapsed_ms: 30_000,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(prepared.owners.len(), 64);
        assert_eq!(prepared.units.len(), 11_200);
        prepared.recheck(&catalog).unwrap();
        let owner_guards = prepared.owner_dependencies().unwrap();
        let mut seen = BTreeSet::new();
        let missing: Vec<_> = prepared
            .units
            .iter()
            .filter(|unit| seen.insert(unit.input_hash.clone()))
            .map(RenderedUnit::input)
            .collect();
        assert_eq!(missing.len(), 11_200);
        let scope = prepared.paid_scope(&prepared.units, &missing, 127).unwrap();
        let supplier_indices: Vec<_> = missing
            .iter()
            .map(|input| {
                let owners = &prepared.input_owners[&input.input_hash];
                assert_eq!(owners.len(), 1);
                assert_eq!(
                    scope.source_bindings[&input.input_hash].as_ref(),
                    owner_guards[owners[0]].1
                );
                owners[0]
            })
            .collect();
        for (_, guards) in &owner_guards {
            for guard in guards {
                let actual = fs_handle
                    .read_before(&guard.path)
                    .unwrap()
                    .map_or(ExpectedState::Absent, |before| {
                        ExpectedState::Hash(before.hash)
                    });
                assert_eq!(actual, guard.expected);
            }
        }
        struct Mock {
            fs: VaultFs,
            vault: RecordId,
            calls: AtomicUsize,
            items: AtomicUsize,
            supplied: InputBindings,
            references: Mutex<BTreeMap<RecordId, BTreeMap<Blake3Hash, BoundedPayloadRef>>>,
            observed: Mutex<Vec<(Blake3Hash, Vec<Blake3Hash>)>>,
        }
        impl Transport for Mock {
            fn execute<'a>(
                &'a self,
                request: AuthenticatedRequest<'a>,
                _: TransportContext,
            ) -> TransportFuture<'a> {
                assert!(
                    self.calls
                        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |count| (count < 32)
                            .then_some(count + 1))
                        .is_ok(),
                    "diagnostic must stop before mock request 33"
                );
                let attempt = request.summary().attempt;
                let cached = self
                    .references
                    .lock()
                    .unwrap()
                    .get(&attempt.run_id)
                    .and_then(|references| references.get(&attempt.task_key))
                    .cloned();
                let reference = cached.unwrap_or_else(|| {
                    let inspection = JobLedger::new(
                        self.fs.clone(),
                        self.vault.clone(),
                        attempt.run_id.clone(),
                        common::options(),
                    )
                    .unwrap()
                    .inspect()
                    .unwrap();
                    let references: BTreeMap<_, _> = inspection
                        .tasks
                        .into_iter()
                        .map(|(key, task)| {
                            assert!(
                                !task.spec.source_bindings.is_empty()
                                    && task.spec.source_bindings.len() <= 127
                            );
                            (key, task.spec.input)
                        })
                        .collect();
                    let reference = references[&attempt.task_key].clone();
                    self.references
                        .lock()
                        .unwrap()
                        .insert(attempt.run_id.clone(), references);
                    reference
                });
                let bytes = fs::read(self.fs.root().path().join(reference.path.as_str())).unwrap();
                assert_eq!(bytes.len() as u64, reference.byte_len);
                assert!(bytes.len() <= 262144);
                assert_eq!(Blake3Hash::digest(&bytes), reference.hash);
                let remote: RemoteInput = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(
                    crate::graph::packet::canonical_json(&remote).unwrap(),
                    bytes
                );
                let RemoteOperation::Embed {
                    inputs,
                    expected_dimensions,
                    ..
                } = remote.operation
                else {
                    panic!("document embeddings only");
                };
                assert_eq!(expected_dimensions, Some(2));
                assert!(!inputs.is_empty() && inputs.len() <= 32);
                let hashes: Vec<_> = inputs
                    .iter()
                    .map(|input| {
                        assert_eq!(Blake3Hash::digest(input.utf8.as_bytes()), input.input_hash);
                        assert!(self.supplied.contains_key(&input.input_hash));
                        input.input_hash.clone()
                    })
                    .collect();
                let mut observed = self.observed.lock().unwrap();
                assert!(observed.iter().all(|(key, _)| key != &attempt.task_key));
                observed.push((attempt.task_key, hashes));
                drop(observed);
                self.items.fetch_add(inputs.len(), Ordering::SeqCst);
                let data: Vec<_> = inputs
                    .iter()
                    .enumerate()
                    .map(|(index, input)| {
                        let vector = if input.utf8.contains("Guardneedle stores 47 cobalt tokens") {
                            vec![1.0, 0.0]
                        } else {
                            vec![0.0, 1.0]
                        };
                        serde_json::json!({"index":index,"embedding":vector})
                    })
                    .collect();
                let reply = TransportReply::new(200, vec![], serde_json::to_vec(&serde_json::json!({
                    "model":"test-model","data":data,"usage":{"prompt_tokens":inputs.len()*2,"total_tokens":inputs.len()*2}
                })).unwrap()).unwrap();
                Box::pin(async move { Ok(reply) })
            }
        }
        struct NoSecrets;
        impl SecretInputs for NoSecrets {
            fn environment(&self, _: &str, _: usize) -> Result<Option<SecretBytes>> {
                Err(WikiError::invalid(
                    "diagnostic forbids external credentials",
                ))
            }
            fn file(&self, _: &Path, _: usize) -> Result<SecretBytes> {
                Err(WikiError::invalid("diagnostic forbids credential files"))
            }
        }
        struct NoHelpers(Arc<AtomicUsize>);
        impl HelperRunner for NoHelpers {
            fn run(
                &self,
                _: &HelperInvocation,
                _: &HelperLimits,
                _: &dyn JobClock,
                _: &CancellationToken,
            ) -> Result<HelperOutput> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Err(WikiError::invalid("diagnostic forbids helpers"))
            }
        }
        struct ZeroJitter;
        impl JitterSource for ZeroJitter {
            fn sample_inclusive(&self, _: u64) -> Result<u64> {
                Ok(0)
            }
        }
        let mock = Arc::new(Mock {
            fs: fs_handle.clone(),
            vault: app.vault_id().clone(),
            calls: AtomicUsize::new(0),
            items: AtomicUsize::new(0),
            supplied: scope.source_bindings.clone(),
            references: Mutex::new(BTreeMap::new()),
            observed: Mutex::new(Vec::new()),
        });
        let helpers = Arc::new(AtomicUsize::new(0));
        let dispatcher = Dispatcher::new(
            fs_handle.clone(),
            DispatchOptions {
                broker: Arc::new(CredentialBroker::new(CredentialOptions {
                    clock: Arc::new(common::TestClock),
                    inputs: Arc::new(NoSecrets),
                    runner: Arc::new(NoHelpers(helpers.clone())),
                })),
                transport: mock.clone(),
                jitter: Arc::new(ZeroJitter),
            },
        );
        let limits = LifetimeLimits {
            requests: 512,
            concurrency: 1,
            attempts_per_task: 1,
            request_bytes: Some(64 * 1024 * 1024),
            response_bytes: Some(4 * 1024 * 1024 * 1024),
            ..Default::default()
        };
        let runtime = EmbeddingRuntime::new(
            &service,
            &dispatcher,
            common::options(),
            limits.clone(),
            1_800_000_000_000,
            1_800_000_120_000,
            None,
        );
        let batches = app
            .embedding_guard_batches(&runtime, &missing, Some(&scope.source_bindings), &[])
            .unwrap();
        let tasks = app
            .embedding_tasks_with_bindings(
                &spec,
                &batches,
                &scope.units,
                &runtime,
                "embeddings_sync",
                false,
                Some(&scope.source_bindings),
            )
            .unwrap();
        assert_eq!(tasks.len(), 350);
        assert!(tasks.iter().all(|task| !task.source_bindings.is_empty()));
        for (task, batch) in tasks.iter().zip(&batches) {
            let bytes = fs::read(destination.join(task.input.path.as_str())).unwrap();
            assert_eq!(bytes.len() as u64, task.input.byte_len);
            assert_eq!(Blake3Hash::digest(&bytes), task.input.hash);
            let remote: RemoteInput = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                crate::graph::packet::canonical_json(&remote).unwrap(),
                bytes
            );
            let RemoteOperation::Embed {
                inputs,
                expected_dimensions,
                ..
            } = remote.operation
            else {
                panic!("embedding task required during preflight");
            };
            assert_eq!(expected_dimensions, Some(2));
            assert_eq!(
                inputs
                    .iter()
                    .map(|input| &input.input_hash)
                    .collect::<Vec<_>>(),
                batch
                    .iter()
                    .map(|input| &input.input_hash)
                    .collect::<Vec<_>>()
            );
        }
        assert!(
            crate::vault::operational::RunStore::discover_existing(
                &fs_handle,
                app.vault_id(),
                4096,
                &mut || Ok(())
            )
            .unwrap()
            .is_empty()
        );
        assert_eq!(
            fs::read_dir(destination.join(".wiki/state/embedding-jobs"))
                .unwrap()
                .count(),
            0
        );
        let store = VectorStore::open(&fs_handle, None).unwrap();
        assert!(store.active().unwrap().is_none());
        let initial_coverage = store
            .coverage(&spec.id().unwrap(), &prepared.units)
            .unwrap();
        assert_eq!(initial_coverage.available_units, 0);
        assert_eq!(initial_coverage.missing_units, 11_200);
        let initial_space = store.space(&spec.id().unwrap()).unwrap().unwrap();
        fn embedding_rows(root: &Path) -> BTreeMap<&'static str, u64> {
            let connection = rusqlite::Connection::open_with_flags(
                root.join(".wiki/cache/embeddings.sqlite3"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap();
            [
                ("embedding_spaces", "SELECT COUNT(*) FROM embedding_spaces"),
                (
                    "embedding_vectors",
                    "SELECT COUNT(*) FROM embedding_vectors",
                ),
                (
                    "embedding_memberships",
                    "SELECT COUNT(*) FROM embedding_memberships",
                ),
                (
                    "embedding_unit_owners",
                    "SELECT COUNT(*) FROM embedding_unit_owners",
                ),
                (
                    "embedding_preparation_cursors",
                    "SELECT COUNT(*) FROM embedding_preparation_cursors",
                ),
                (
                    "active_spaces",
                    "SELECT COUNT(*) FROM embedding_spaces WHERE active=1",
                ),
            ]
            .into_iter()
            .map(|(name, sql)| {
                (
                    name,
                    connection
                        .query_row(sql, [], |row| row.get::<_, i64>(0))
                        .unwrap()
                        .try_into()
                        .unwrap(),
                )
            })
            .collect()
        }
        let initial_rows = embedding_rows(&destination);
        assert_eq!(initial_rows["embedding_spaces"], 1);
        for (table, count) in &initial_rows {
            if *table != "embedding_spaces" {
                assert_eq!(*count, 0, "{table}");
            }
        }
        drop(store);
        let scope_commitment = Blake3Hash::digest(
            crate::graph::packet::canonical_json(
                &scope
                    .units
                    .iter()
                    .map(|unit| {
                        (
                            &unit.unit_id,
                            &unit.input_hash,
                            &unit.dependency_fingerprint,
                        )
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap(),
        );
        let post_preparation_files = files(&destination);
        let all_post_setup_hash = Blake3Hash::digest(
            crate::graph::packet::canonical_json(&post_preparation_files).unwrap(),
        );
        let writer_pid_diagnostic = post_preparation_files.get(".wiki/state/writer.lock");
        assert!(writer_pid_diagnostic.is_some());
        let protocol = serde_json::json!({"version":1,"geometry":settings,"service":service.summary(),
            "space_id":spec.id().unwrap(),"vault_id":app.vault_id(),"limits":limits,"deadline_ms":120000,
            "pristine_durable_files":pristine_files,"initial_space":initial_space,"initial_embedding_rows":initial_rows,
            "physical_setup_diagnostics":"per-arm only: SQLite page/WAL salts, writer PID and shm locks may differ",
            "ordered_input_hashes":missing.iter().map(|input| &input.input_hash).collect::<Vec<_>>(),
            "ordered_task_keys":tasks.iter().map(|task| &task.key).collect::<Vec<_>>(),
            "ordered_staged_task_inputs":tasks.iter().map(|task| &task.input).collect::<Vec<_>>(),
            "ordered_task_guard_commitments":tasks.iter().map(|task| Blake3Hash::digest(crate::graph::packet::canonical_json(&task.source_bindings).unwrap())).collect::<Vec<_>>(),
            "supplier_owner_for_input":supplier_indices,"complete_owner_guards":owner_guards,
            "scope_commitment":scope_commitment,"initial_vectors":0,"initial_owner_ack":0,"initial_jobs":0,
            "setup_effects":"identical immutable task staging and empty prepared space; setup elapsed included once"});
        if arm == Arm::Exact {
            write_json(&protocol_path, &protocol);
        } else {
            let bytes = fs::read(&protocol_path).unwrap();
            assert!(bytes.len() <= 2 * 1024 * 1024);
            let frozen: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                protocol, frozen,
                "paired tasks, suppliers, service and initial setup must match before probe"
            );
        }
        let protocol_hash = Blake3Hash::digest(fs::read(&protocol_path).unwrap());
        let preparation_ns = preflight_started.elapsed().as_nanos();
        let preflight_bytes = write_json(
            &output.join("preflight.json"),
            &serde_json::json!({
            "arm":arm,"protocol_hash":protocol_hash,"prepared_elapsed_ns":preparation_ns,"elapsed_already_in_probe":true,
            "inputs":11200,"tasks":350,"copied_bytes":copied_bytes,"service":service.summary(),"initial_coverage":initial_coverage,
            "all_post_setup_files_hash":all_post_setup_hash,"post_preparation_durable_files":post_preparation_files,
            "physical_files_are_diagnostic_only":true,"writer_pid_diagnostic_hash":writer_pid_diagnostic,
            "initial_embedding_rows":initial_rows,"initial_space":initial_space}),
        );
        // Audit work begins only after with_probe has disabled the observer.
        let (result, report) = probe::with_probe(arm, preflight_started, || {
            app.embeddings_sync(&settings, &runtime)
        });
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("diagnostic must not manufacture complete sync"),
        };
        assert!(report.completed_tasks.len() <= 32);
        assert!(report.diagnostic_stop.is_some());
        assert!(!report.instrumentation_overflow && report.open_stages == 0);
        let observed = mock.observed.lock().unwrap().clone();
        assert_eq!(mock.calls.load(Ordering::SeqCst), observed.len());
        assert!(observed.len() <= 32);
        for ((key, inputs), task) in observed.iter().zip(&tasks) {
            assert_eq!(key, &task.key);
            let reference = &task.input;
            let bytes = fs::read(destination.join(reference.path.as_str())).unwrap();
            let remote: RemoteInput = serde_json::from_slice(&bytes).unwrap();
            let RemoteOperation::Embed {
                inputs: frozen_inputs,
                ..
            } = remote.operation
            else {
                panic!("embedding task required");
            };
            assert_eq!(
                inputs,
                &frozen_inputs
                    .into_iter()
                    .map(|input| input.input_hash)
                    .collect::<Vec<_>>()
            );
        }
        for (completed, task) in report.completed_tasks.iter().zip(&tasks) {
            assert_eq!(completed.task_key, task.key.as_str());
        }
        let runs = crate::vault::operational::RunStore::discover_existing(
            &fs_handle,
            app.vault_id(),
            4096,
            &mut || Ok(()),
        )
        .unwrap();
        let frozen_tasks: BTreeMap<_, _> = tasks.iter().map(|task| (&task.key, task)).collect();
        let mut actual_completed = BTreeSet::new();
        let mut admitted = BTreeSet::new();
        let mut audits = Vec::new();
        for run in &runs {
            let inspection = JobLedger::new(
                fs_handle.clone(),
                app.vault_id().clone(),
                run.clone(),
                common::options(),
            )
            .unwrap()
            .inspect()
            .unwrap();
            assert_eq!(inspection.spec.scope.operation, "embeddings_sync");
            assert_eq!(inspection.spec.limits, limits);
            assert_eq!(inspection.spec.deadline_utc_ms, runtime.deadline_utc_ms);
            JobLedger::validate_bootstrap_size(&inspection.spec).unwrap();
            for task in inspection.tasks.values() {
                assert!(admitted.insert(task.spec.key.clone()));
                assert_eq!(&task.spec, frozen_tasks[&task.spec.key]);
                let guard_union: BTreeMap<_, _> = batches[tasks
                    .iter()
                    .position(|frozen| frozen.key == task.spec.key)
                    .unwrap()]
                .iter()
                .flat_map(|input| mock.supplied[&input.input_hash].iter())
                .map(|guard| (&guard.path, &guard.expected))
                .collect();
                assert_eq!(
                    task.spec
                        .source_bindings
                        .iter()
                        .map(|guard| (&guard.path, &guard.expected))
                        .collect::<BTreeMap<_, _>>(),
                    guard_union
                );
                if task.state == TaskState::Completed {
                    actual_completed.insert(task.spec.key.clone());
                }
            }
            let task_hashes: BTreeSet<_> = inspection
                .tasks
                .keys()
                .flat_map(|key| {
                    &batches[tasks.iter().position(|frozen| &frozen.key == key).unwrap()]
                })
                .map(|input| &input.input_hash)
                .collect();
            let expected_scope = Blake3Hash::digest(
                crate::graph::packet::canonical_json(
                    &scope
                        .units
                        .iter()
                        .filter(|unit| task_hashes.contains(&unit.input_hash))
                        .map(|unit| {
                            (
                                &unit.unit_id,
                                &unit.input_hash,
                                &unit.dependency_fingerprint,
                            )
                        })
                        .collect::<Vec<_>>(),
                )
                .unwrap(),
            );
            assert_eq!(
                inspection.spec.scope.scope_payload_hash,
                Some(expected_scope)
            );
            for attempt in &inspection.attempts {
                if let Some(spool) = &attempt.spool {
                    for reference in [&spool.response, &spool.metadata] {
                        let bytes = fs::read(destination.join(reference.path.as_str())).unwrap();
                        assert_eq!(bytes.len() as u64, reference.byte_len);
                        assert_eq!(Blake3Hash::digest(&bytes), reference.hash);
                    }
                }
                if actual_completed.contains(&attempt.attempt.task_key) {
                    assert_eq!(attempt.phase, AttemptPhase::Settled);
                    let receipt_ref = attempt.receipt.as_ref().unwrap();
                    let receipt =
                        crate::jobs::checkpoint::receipt(&fs_handle, receipt_ref).unwrap();
                    assert_eq!(receipt.output_disposition, OutputDisposition::Validated);
                    let committed = crate::jobs::checkpoint::named_job_committed(
                        &fs_handle,
                        app.vault_id(),
                        run,
                        crate::jobs::checkpoint::JobPublicationKey::Receipt {
                            receipt_id: receipt.receipt_id,
                        },
                        &receipt_ref.path,
                        &receipt_ref.hash,
                    )
                    .unwrap()
                    .unwrap();
                    let (manifest, hash) = crate::changes::ChangeEngine::new(fs_handle.clone())
                        .unwrap()
                        .load_manifest(&committed.change_id)
                        .unwrap();
                    assert_eq!(hash, committed.manifest_hash);
                    let guards = &inspection.tasks[&attempt.attempt.task_key]
                        .spec
                        .source_bindings;
                    assert!(
                        guards
                            .iter()
                            .all(|guard| manifest.read_preconditions.contains(guard))
                    );
                    assert_eq!(manifest.read_preconditions.len(), guards.len() + 1);
                    assert!(manifest.read_preconditions.len() <= 128);
                }
            }
            let occurrences = inspection
                .tasks
                .values()
                .map(|task| task.spec.source_bindings.len())
                .sum::<usize>();
            if arm == Arm::GuardOccurrences127 {
                assert!(occurrences <= 127);
            }
            audits.push(serde_json::json!({"run_id":run,"state":inspection.state,"tasks":inspection.tasks.len(),
                "guard_occurrences":occurrences,"spec_json_bytes":serde_json::to_vec(&inspection.spec).unwrap().len(),
                "budget":inspection.budget,"attempts":inspection.attempts.iter().map(|attempt| {
                    let receipt = attempt.receipt.as_ref().map(|reference| crate::jobs::checkpoint::receipt(&fs_handle, reference).unwrap());
                    serde_json::json!({
                    "task_key":attempt.attempt.task_key,"attempt":attempt.attempt.attempt_id,"phase":attempt.phase,
                    "billing":attempt.billing,"remote_exposure":attempt.remote_exposure,"bound":attempt.bound,
                    "allowance":attempt.allowance,"persistence":attempt.persistence,
                    "unknown_hold":inspection.budget.unknown_attempts.contains(&attempt.attempt.attempt_id),
                    "receipt_computed_cost":receipt.as_ref().map(|receipt| &receipt.computed_cost),
                    "receipt_usage":receipt.as_ref().map(|receipt| &receipt.usage),
                    "cache_outputs":attempt.cache_outputs.len()})
                }).collect::<Vec<_>>() }));
        }
        let recorded_completed: BTreeSet<_> = report
            .completed_tasks
            .iter()
            .map(|task| Blake3Hash::new(task.task_key.as_str()).unwrap())
            .collect();
        assert_eq!(actual_completed, recorded_completed);
        for (_, guards) in &owner_guards {
            for guard in guards {
                assert_eq!(
                    fs_handle
                        .read_before(&guard.path)
                        .unwrap()
                        .map_or(ExpectedState::Absent, |before| ExpectedState::Hash(
                            before.hash
                        )),
                    guard.expected
                );
            }
        }
        let store = VectorStore::open(&fs_handle, None).unwrap();
        assert!(store.active().unwrap().is_none());
        let coverage = store
            .coverage(&spec.id().unwrap(), &prepared.units)
            .unwrap();
        assert!(coverage.available_units < 11_200 && coverage.missing_units > 0);
        let final_rows = embedding_rows(&destination);
        assert_eq!(final_rows["embedding_unit_owners"], 0);
        assert_eq!(final_rows["embedding_preparation_cursors"], 0);
        assert_eq!(final_rows["embedding_memberships"], 0);
        assert_eq!(final_rows["active_spaces"], 0);
        drop(store);
        assert_eq!(helpers.load(Ordering::SeqCst), 0);
        let artifact_bytes = files(&destination)
            .values()
            .map(|(length, _)| *length)
            .sum::<u64>();
        assert!(artifact_bytes <= 256 * 1024 * 1024);
        let completed_inputs: Vec<_> = observed
            .iter()
            .take(report.completed_tasks.len())
            .flat_map(|(_, hashes)| hashes.clone())
            .collect();
        let diagnostic_bytes = write_json(
            &output.join("diagnostic.json"),
            &serde_json::json!({
            "arm":arm,"protocol_hash":protocol_hash,"report":report,"preparation_ns":preparation_ns,"preparation_already_included":true,
            "terminal_error":error,"job_audits":audits,"completed_input_hashes":completed_inputs,"mock_calls":observed.len(),
            "mock_items":mock.items.load(Ordering::SeqCst),"coverage_is_partial":coverage,"final_embedding_rows":final_rows,"owner_ack":false,"query_ready":false,
            "artifact_bytes":artifact_bytes,"physical_io_bytes":"unavailable","peak_allocation_bytes":"unavailable",
            "audit_outside_probe":true,"no_resend":true,"helper_calls":0}),
        );
        assert!(
            report.trace_bytes
                + preflight_bytes
                + diagnostic_bytes
                + fs::metadata(&protocol_path).unwrap().len() as usize
                <= 8 * 1024 * 1024
        );
    }
}

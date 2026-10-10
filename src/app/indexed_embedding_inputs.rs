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
    time::{Duration, Instant},
};

/// A newly declared paid scope. Reuse acknowledgment still authenticates all
/// owners; these units and guards identify only the suppliers of paid inputs.
pub(super) struct PaidEmbeddingScope {
    pub units: Vec<RenderedUnit>,
    pub source_bindings: BTreeMap<Blake3Hash, Vec<ReadDependency>>,
}

const MAX_OWNERS: usize = 4096;
const MAX_PATH_BYTES: usize = 8 * 1024 * 1024;
const MAX_BINDING_ENTRIES: usize = 65_536;
const MAX_BINDING_BYTES: usize = 64 * 1024 * 1024;
pub(super) const PROOF_SCOPE_OWNERS: usize = 16;

/// Detached planning observations. These guards and rendered inputs cannot
/// authorize a later send or publication without a new canonical proof.
pub(super) struct EmbeddingPlanningSummary {
    pub snapshot: ReadSnapshot,
    pub units: Vec<RenderedUnit>,
    owners: BTreeMap<VaultRelativePath, Vec<ReadDependency>>,
}
impl EmbeddingPlanningSummary {
    pub(super) fn new(snapshot: ReadSnapshot) -> Self {
        Self {
            snapshot,
            units: Vec::new(),
            owners: BTreeMap::new(),
        }
    }
    pub(super) fn append(&mut self, summary: Self) -> Result<()> {
        if self.snapshot != summary.snapshot {
            return Err(conflict(
                "publication changed between preparation planning scopes",
            ));
        }
        if self.owners.len().saturating_add(summary.owners.len()) > 128
            || self.units.len().saturating_add(summary.units.len()) > 65_536
        {
            return Err(exhausted(
                "preparation planning page owner/unit allowance exhausted",
            ));
        }
        let mut render_bytes = 0usize;
        for unit in self.units.iter().chain(&summary.units) {
            render_bytes = render_bytes
                .checked_add(unit.utf8.len())
                .filter(|bytes| *bytes <= 64 * 1024 * 1024)
                .ok_or_else(|| exhausted("preparation planning page render allowance exhausted"))?;
        }
        let mut inventory = BindingInventory::default();
        for guards in self.owners.values().chain(summary.owners.values()) {
            for guard in guards {
                inventory.reserve(1, guard_bytes(&guard.path, &guard.expected)?)?;
            }
        }
        if summary
            .owners
            .keys()
            .any(|owner| self.owners.contains_key(owner))
        {
            return Err(conflict(
                "preparation planning owner repeated across scopes",
            ));
        }
        self.units.extend(summary.units);
        self.owners.extend(summary.owners);
        Ok(())
    }
    pub(super) fn paid_scope(
        &self,
        missing: &[crate::providers::types::EmbeddingInput],
        available_guards: usize,
        deadline: Instant,
    ) -> Result<PaidEmbeddingScope> {
        paid_scope_from_owners(
            &self.units,
            missing,
            available_guards,
            self.owners
                .iter()
                .map(|(owner, guards)| (owner, guards.as_slice())),
            || {
                if Instant::now() < deadline {
                    Ok(())
                } else {
                    Err(exhausted("preparation planning command deadline exceeded"))
                }
            },
        )
    }
}

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
#[derive(Default)]
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

fn paid_scope_from_owners<'a>(
    units: &[RenderedUnit],
    missing: &[crate::providers::types::EmbeddingInput],
    available_guards: usize,
    owners: impl IntoIterator<Item = (&'a VaultRelativePath, &'a [ReadDependency])>,
    check_deadline: impl Fn() -> Result<()>,
) -> Result<PaidEmbeddingScope> {
    check_deadline()?;
    let wanted: BTreeSet<_> = missing.iter().map(|input| &input.input_hash).collect();
    let mut by_owner: BTreeMap<_, BTreeMap<_, Vec<_>>> = BTreeMap::new();
    for unit in units
        .iter()
        .filter(|unit| wanted.contains(&unit.input_hash))
    {
        check_deadline()?;
        by_owner
            .entry(&unit.owner)
            .or_default()
            .entry(&unit.input_hash)
            .or_default()
            .push(unit);
    }
    let mut suppliers = BTreeMap::new();
    let mut inventory = BindingInventory::default();
    let mut oversized = BTreeMap::new();
    // Inspect path-ordered supplier observations before freezing a task.
    // Each per-input guard copy is metered before allocation; duplicate
    // supplier observations do not enlarge the chosen paid guard union.
    for (path, guards) in owners {
        check_deadline()?;
        let Some(owner_units) = by_owner.get(path) else {
            continue;
        };
        for hash in owner_units.keys().copied() {
            check_deadline()?;
            if guards.len() <= available_guards {
                if !suppliers.contains_key(hash) {
                    let key_bytes = size_of::<(Blake3Hash, Vec<ReadDependency>)>()
                        .checked_add(hash.as_str().len())
                        .ok_or_else(|| exhausted("normalized embedding input key byte overflow"))?;
                    inventory.reserve(1, key_bytes)?;
                    for guard in guards {
                        check_deadline()?;
                        inventory.reserve(1, guard_bytes(&guard.path, &guard.expected)?)?;
                    }
                    suppliers.insert(hash.clone(), (path.clone(), guards.to_vec()));
                }
            } else {
                let candidate = oversized
                    .entry(hash.clone())
                    .or_insert((path.clone(), guards.len()));
                if guards.len() < candidate.1 {
                    *candidate = (path.clone(), guards.len());
                }
            }
        }
    }
    let mut scope = PaidEmbeddingScope {
        units: Vec::new(),
        source_bindings: BTreeMap::new(),
    };
    for input in missing {
        check_deadline()?;
        let Some((owner, guards)) = suppliers.remove(&input.input_hash) else {
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
        scope.units.extend(
            by_owner[&owner][&input.input_hash]
                .iter()
                .map(|unit| (**unit).clone()),
        );
        scope
            .source_bindings
            .insert(input.input_hash.clone(), guards);
    }
    Ok(scope)
}

pub(super) struct NormalizedEmbeddingInputs {
    pub snapshot: ReadSnapshot,
    /// All exact owner units remain present, including shared request inputs.
    pub units: Vec<RenderedUnit>,
    /// Unique provider input hashes with the union of every matching owner's guards.
    pub source_bindings: BTreeMap<Blake3Hash, Vec<ReadDependency>>,
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
        let owners = self.owner_dependencies()?;
        paid_scope_from_owners(
            units,
            missing,
            available_guards,
            owners
                .iter()
                .map(|(owner, guards)| (owner, guards.as_slice())),
            || self.allowance.milliseconds().map(|_| ()),
        )
    }
    pub(super) fn into_planning_summary(self) -> Result<EmbeddingPlanningSummary> {
        self.allowance.milliseconds()?;
        if self.failed {
            return Err(conflict("failed proof cannot produce a planning summary"));
        }
        let owners = self.owner_dependencies()?.into_iter().collect();
        Ok(EmbeddingPlanningSummary {
            snapshot: self.snapshot,
            units: self.units,
            owners,
        })
    }
    pub(super) fn owner_dependencies(
        &self,
    ) -> Result<Vec<(VaultRelativePath, Vec<ReadDependency>)>> {
        let mut entries = 0usize;
        let mut bytes = 0usize;
        let mut out = Vec::new();
        for owner in &self.owners {
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
        let mut seen_inputs = BTreeSet::new();
        for unit in &owner.units {
            self.allowance.milliseconds()?;
            if !seen_inputs.insert(&unit.input_hash) {
                continue;
            }
            if !self.source_bindings.contains_key(&unit.input_hash) {
                let key_bytes = size_of::<(Blake3Hash, Vec<ReadDependency>)>()
                    .checked_add(unit.input_hash.as_str().len())
                    .ok_or_else(|| exhausted("normalized embedding input key byte overflow"))?;
                self.inventory.reserve(1, key_bytes)?;
                self.source_bindings
                    .insert(unit.input_hash.clone(), Vec::new());
            }
            let guards = self
                .source_bindings
                .get_mut(&unit.input_hash)
                .expect("reserved input key");
            // Borrow before accounting; no allocating read_preconditions() call.
            for (path, expected) in owner.proof.read_preconditions_iter() {
                self.allowance.milliseconds()?;
                match guards.binary_search_by(|guard| guard.path.cmp(path)) {
                    Ok(index) if &guards[index].expected != expected => {
                        return Err(conflict(
                            "matching embedding inputs have conflicting owner read guards",
                        ));
                    }
                    Ok(_) => {}
                    Err(index) => {
                        self.inventory.reserve(1, guard_bytes(path, expected)?)?;
                        guards.try_reserve_exact(1).map_err(|_| {
                            exhausted("normalized embedding guard allocation refused")
                        })?;
                        guards.insert(
                            index,
                            ReadDependency {
                                path: path.clone(),
                                expected: expected.clone(),
                            },
                        );
                    }
                }
            }
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
    /// A failed batch cannot be retried with partially consumed proof allowances.
    /// Callers stop on any failure and never emit/commit the retained inputs.
    pub(super) fn recheck(&mut self, catalog: &Catalog) -> Result<()> {
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
        source_bindings: BTreeMap::new(),
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
    result.reader.verify_operations(catalog)?;
    result.allowance.milliseconds()?;
    #[cfg(test)]
    {
        observed.complete = true;
    }
    Ok(result)
}

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
    #[test]
    fn captured_owner_page_preserves_cumulative_row_and_canonical_limits() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("WIKI.md"), "---\nwiki_schema: '1'\nwiki_id: vault_capture_page\nwiki_kind: vault\ntitle: Captured owner page\n---\n").unwrap();
        let handle = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let mut paths = Vec::new();
        for index in 0..128 {
            let plan = SourceStore::new(handle.clone())
                .plan_capture(CaptureRequest {
                    title: format!("Captured owner {index}"),
                    origin_kind: SourceOrigin::LocalFile,
                    origin: format!("fixture-{index}.md"),
                    original: format!("Exact captured evidence {index}.\n").into_bytes(),
                    extraction: ExtractionInput::Utf8Preserve,
                    media_type: None,
                })
                .unwrap();
            paths.push(
                plan.draft
                    .as_ref()
                    .unwrap()
                    .operations
                    .iter()
                    .find(|op| op.target.as_str().ends_with("content.md"))
                    .unwrap()
                    .target
                    .clone(),
            );
            apply(temp.path(), plan);
        }
        let catalog = Catalog::new(handle, RecordId::new("vault_capture_page").unwrap());
        let writer = WriterPermit::acquire(catalog.fs().root(), Duration::from_secs(1)).unwrap();
        catalog.rebuild_normalized(&writer).unwrap();
        drop(writer);
        let settings = EmbeddingSettings {
            max_input_bytes: 8000,
            ..Default::default()
        };
        let mut inputs = materialize(
            &catalog,
            &settings,
            Some(&paths),
            &VerificationBudget::default(),
        )
        .unwrap();
        assert_eq!(inputs.owners.len(), 128);
        assert_eq!(inputs.units.len(), 128);
        assert_eq!(
            inputs
                .units
                .iter()
                .map(|u| &u.owner)
                .collect::<BTreeSet<_>>(),
            paths.iter().collect::<BTreeSet<_>>()
        );
        // These ordinary Source/Revision/Vault closures previously performed
        // 46 selected-row reads each (5888), exceeding the shared4096 ceiling.
        // The pinned layout is now read once; all31 other rows per owner,
        // including both identity schema witnesses per record, remain charged.
        assert_eq!(inputs.reader.usage().rows, 3969);
        let before = inputs.allowance.remaining.max_files;
        // A third full canonical pass over 128 owners exceeds the unchanged
        // 16,384-entry allowance. Ordinary preparation must use smaller scopes.
        assert_eq!(code(inputs.recheck(&catalog)), ErrorCode::BudgetExceeded);
        assert_eq!(inputs.reader.usage().rows, 3969);
        assert!(inputs.allowance.remaining.max_files < before);
        assert_eq!(
            code(inputs.into_planning_summary()),
            ErrorCode::FreshnessConflict
        );
        assert_eq!(
            code(materialize_with_query_limits(
                &catalog,
                &settings,
                Some(&paths),
                &VerificationBudget::default(),
                QueryReadLimits {
                    max_rows: 3968,
                    ..Default::default()
                },
            )),
            ErrorCode::BudgetExceeded
        );
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
        let guards = &full.source_bindings[&captured_units[0].input_hash];
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
            full.source_bindings[&page.input_hash]
                .iter()
                .any(|guard| guard.path == path("entity.md"))
        );
        let plain = full
            .units
            .iter()
            .find(|unit| unit.owner == path("plain-a.md"))
            .unwrap();
        let guards = &full.source_bindings[&plain.input_hash];
        assert!(guards.iter().any(|guard| guard.path == path("plain-a.md")));
        assert!(guards.iter().any(|guard| guard.path == path("plain-b.md")));
        assert!(guards.windows(2).all(|pair| pair[0].path < pair[1].path));
        assert!(full.source_bindings.len() < full.units.len());
        // Count repeated guard copies across distinct input keys explicitly.
        let expected_entries =
            full.source_bindings.len() + full.source_bindings.values().map(Vec::len).sum::<usize>();
        assert_eq!(full.inventory.entries, expected_entries);
    }
    #[test]
    fn detached_planning_deduplicates_across_scopes_and_selects_feasible_supplier() {
        let fixture = Fixture::new();
        let mut first = fixture.materialize(Some(&[path("plain-a.md")]));
        first.recheck(&fixture.catalog).unwrap();
        let mut first = first.into_planning_summary().unwrap();
        let mut second = fixture.materialize(Some(&[path("plain-b.md")]));
        second.recheck(&fixture.catalog).unwrap();
        let second = second.into_planning_summary().unwrap();
        assert_eq!(first.units[0].input_hash, second.units[0].input_hash);
        let missing = vec![first.units[0].input()];
        let available_guards = second.owners[&path("plain-b.md")].len();
        // A detached summary is an observation, not proof authority. Model a
        // larger supplier closure here; later paid admission must authenticate
        // the selected supplier independently against its actual task guards.
        first
            .owners
            .get_mut(&path("plain-a.md"))
            .unwrap()
            .push(ReadDependency {
                path: path("extra-dependency.md"),
                expected: ExpectedState::Absent,
            });
        let mut page = EmbeddingPlanningSummary::new(first.snapshot.clone());
        page.append(first).unwrap();
        assert_eq!(
            code(page.paid_scope(
                &missing,
                available_guards,
                Instant::now() + Duration::from_secs(1)
            )),
            ErrorCode::BudgetExceeded
        );
        page.append(second).unwrap();
        let paid = page
            .paid_scope(
                &missing,
                available_guards,
                Instant::now() + Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(paid.source_bindings.len(), 1);
        assert_eq!(paid.units.len(), 1);
        assert_eq!(paid.units[0].owner, path("plain-b.md"));
        assert_eq!(
            paid.source_bindings[&missing[0].input_hash].len(),
            available_guards
        );
        let repeated = fixture
            .materialize(Some(&[path("plain-b.md")]))
            .into_planning_summary()
            .unwrap();
        assert_eq!(code(page.append(repeated)), ErrorCode::FreshnessConflict);
        let mut changed = fixture
            .materialize(Some(&[path("plain-a.md")]))
            .into_planning_summary()
            .unwrap();
        changed.snapshot.generation += 1;
        assert_eq!(code(page.append(changed)), ErrorCode::FreshnessConflict);
        assert_eq!(
            page.units.len(),
            2,
            "failed append must preserve the original planning page"
        );
    }

    #[test]
    fn detached_paid_planning_retains_deadline_and_per_input_guard_copy_limits() {
        let fixture = Fixture::new();
        let mut inputs = fixture.materialize(Some(&[path("plain-a.md")]));
        inputs.recheck(&fixture.catalog).unwrap();
        let mut page = inputs.into_planning_summary().unwrap();
        let missing = vec![page.units[0].input()];
        assert_eq!(
            code(page.paid_scope(&missing, 127, Instant::now())),
            ErrorCode::BudgetExceeded
        );
        let guards = page.owners.get_mut(&path("plain-a.md")).unwrap();
        while guards.len() < 127 {
            guards.push(ReadDependency {
                path: path(&format!("dependency-{}.md", guards.len())),
                expected: ExpectedState::Absent,
            });
        }
        // One bounded owner observation can supply many different input hashes.
        // Its original 127 guards fit, but copying them for 600 paid inputs does
        // not fit the unchanged cumulative 65,536-entry inventory.
        let template = page.units[0].clone();
        page.units = (0..600)
            .map(|index| {
                let mut unit = template.clone();
                unit.utf8 = format!("distinct planning input {index}");
                unit.input_hash = Blake3Hash::digest(unit.utf8.as_bytes());
                unit
            })
            .collect();
        let missing: Vec<_> = page.units.iter().map(RenderedUnit::input).collect();
        assert_eq!(
            code(page.paid_scope(&missing, 127, Instant::now() + Duration::from_secs(2))),
            ErrorCode::BudgetExceeded
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
        assert_eq!(before.source_bindings, after.source_bindings);
        fixture.publish(true);
        let rebuilt = fixture.materialize(None);
        assert_eq!(before.units, rebuilt.units);
        assert_eq!(before.source_bindings, rebuilt.source_bindings);
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
}

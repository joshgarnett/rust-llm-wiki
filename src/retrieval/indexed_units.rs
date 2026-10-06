//! Bounded normalized document discovery and exact selected-owner rendering.
//! Cached units are hints; only a retained, finally rechecked owner proof can
//! authorize membership or emitted evidence. No legacy projection is rebuilt.
use super::{
    context_types::VerificationBudget,
    render::{self, RenderedUnit},
    selected_documents::{self, SelectedDocuments},
    spaces::EmbeddingSettings,
};
use crate::{
    catalog::{
        Catalog, DocumentRow,
        query::QuerySnapshot,
        query_types::{QueryCatalog, QueryReadUsage},
    },
    changes::ReadDependency,
    domain::*,
};
use std::{
    cell::Cell,
    collections::VecDeque,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug)]
pub(crate) struct UnitLimits {
    pub max_render_bytes: usize,
    pub max_units: usize,
    pub max_owner_render_bytes: usize,
}
impl Default for UnitLimits {
    fn default() -> Self {
        Self {
            max_render_bytes: 64 * 1024 * 1024,
            max_units: 65_536,
            max_owner_render_bytes: 8 * 1024 * 1024,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct UnitUsage {
    pub units: usize,
    pub render_bytes: usize,
    /// Pinned cache work (including scalar pages), never canonical proof work.
    pub source_rows: usize,
    pub source_bytes: usize,
}
/// One operation's cumulative render/decode accounting and monotonic deadline.
/// Reuse this value for both exact-stream replay passes and owner rendering.
/// Cell accounting supports exact_stream's Fn replay factory without resetting
/// its allowance. This is local cooperative accounting, not a parallel reader.
pub(crate) struct UnitBudget {
    limits: UnitLimits,
    deadline: Instant,
    usage: Cell<UnitUsage>,
}
fn budget_error(message: &str) -> WikiError {
    WikiError::new(ErrorCode::BudgetExceeded, message)
}
impl UnitBudget {
    pub(crate) fn new(limits: UnitLimits) -> Result<Self> {
        Self::with_deadline(limits, Instant::now() + Duration::from_secs(30))
    }
    pub(crate) fn with_deadline(limits: UnitLimits, deadline: Instant) -> Result<Self> {
        let ceiling = UnitLimits::default();
        if limits.max_render_bytes == 0
            || limits.max_render_bytes > ceiling.max_render_bytes
            || limits.max_units == 0
            || limits.max_units > ceiling.max_units
            || limits.max_owner_render_bytes == 0
            || limits.max_owner_render_bytes > ceiling.max_owner_render_bytes
            || deadline.saturating_duration_since(Instant::now()) > Duration::from_secs(30)
        {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "document unit budget exceeds its ceiling",
            ));
        }
        let budget = Self {
            limits,
            deadline,
            usage: Cell::new(UnitUsage::default()),
        };
        budget.check()?;
        Ok(budget)
    }
    pub(crate) fn usage(&self) -> UnitUsage {
        self.usage.get()
    }
    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }
    fn check(&self) -> Result<()> {
        if Instant::now() >= self.deadline {
            return Err(budget_error("document unit shared deadline exceeded"));
        }
        Ok(())
    }
    fn remaining_ms(&self) -> Result<u64> {
        self.check()?;
        let remaining = self
            .deadline
            .saturating_duration_since(Instant::now())
            .as_millis();
        let remaining = u64::try_from(remaining).unwrap_or(u64::MAX);
        if remaining == 0 {
            return Err(budget_error("document unit shared deadline exceeded"));
        }
        Ok(remaining)
    }
    fn source_work(&self, before: QueryReadUsage, after: QueryReadUsage) {
        // QuerySnapshot itself bounds these cumulative counts and never refunds.
        let mut usage = self.usage.get();
        usage.source_rows += after.rows.saturating_sub(before.rows);
        usage.source_bytes += after.bytes.saturating_sub(before.bytes);
        self.usage.set(usage);
    }
    fn reserve_unit(&self, bytes: usize, owner_bytes: &mut usize) -> Result<()> {
        self.check()?;
        let mut usage = self.usage.get();
        let owner = owner_bytes
            .checked_add(bytes)
            .filter(|n| *n <= self.limits.max_owner_render_bytes);
        let total = usage
            .render_bytes
            .checked_add(bytes)
            .filter(|n| *n <= self.limits.max_render_bytes);
        let units = usage
            .units
            .checked_add(1)
            .filter(|n| *n <= self.limits.max_units);
        if owner.is_none() || total.is_none() || units.is_none() {
            return Err(budget_error(
                "document render byte/unit reservation exhausted",
            ));
        }
        *owner_bytes = owner.unwrap();
        usage.render_bytes = total.unwrap();
        usage.units = units.unwrap();
        self.usage.set(usage);
        Ok(())
    }
}
fn require_document(document: &DocumentRow) -> Result<()> {
    if document.eligibility != Eligibility::Current
        || !matches!(document.kind, None | Some(RecordKind::Page))
    {
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "embedding units require a current Page, unmanaged note or captured document",
        ));
    }
    Ok(())
}
pub(crate) fn render_owner(
    document: &DocumentRow,
    settings: &EmbeddingSettings,
    fingerprint: Blake3Hash,
    budget: &UnitBudget,
) -> Result<Vec<RenderedUnit>> {
    require_document(document)?;
    budget.check()?;
    let mut units = Vec::new();
    let mut owner_bytes = 0;
    for unit in render::render_document_with_fingerprint(document, settings, fingerprint)? {
        let unit = unit?;
        // The renderer creates at most one bounded unit before this reservation;
        // reserve its actual bytes/unit before retaining it in the owner's vec.
        budget.reserve_unit(unit.utf8.len(), &mut owner_bytes)?;
        units.push(unit);
    }
    budget.check()?;
    Ok(units)
}

/// Replay factory bound to one reader; it cannot authenticate corpus membership.
pub(crate) struct CachedDocumentUnits<'a> {
    reader: &'a QuerySnapshot,
    settings: &'a EmbeddingSettings,
}
impl<'a> CachedDocumentUnits<'a> {
    pub(crate) fn new(reader: &'a QuerySnapshot, settings: &'a EmbeddingSettings) -> Result<Self> {
        settings.validate()?;
        if !reader.normalized_layout() {
            return Err(WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "cached document units require a normalized catalog",
            ));
        }
        Ok(Self { reader, settings })
    }
    pub(crate) fn replay<'b>(&'b self, budget: &'b UnitBudget) -> Result<CachedUnitIter<'b>> {
        budget.check()?;
        Ok(CachedUnitIter {
            reader: self.reader,
            settings: self.settings,
            budget,
            paths: VecDeque::new(),
            after: None,
            last_page: false,
            owner: Vec::new().into_iter(),
            failed: false,
        })
    }
}
pub(crate) struct CachedUnitIter<'a> {
    reader: &'a QuerySnapshot,
    settings: &'a EmbeddingSettings,
    budget: &'a UnitBudget,
    paths: VecDeque<VaultRelativePath>,
    after: Option<VaultRelativePath>,
    last_page: bool,
    owner: std::vec::IntoIter<RenderedUnit>,
    failed: bool,
}
impl CachedUnitIter<'_> {
    fn next_unit(&mut self) -> Result<Option<RenderedUnit>> {
        loop {
            self.budget.check()?;
            if let Some(unit) = self.owner.next() {
                return Ok(Some(unit));
            }
            // Release the exhausted owner's allocation before hydrating another.
            self.owner = Vec::new().into_iter();
            if let Some(path) = self.paths.pop_front() {
                let before = self.reader.usage();
                let result = self.reader.document(&path);
                self.budget.source_work(before, self.reader.usage());
                let document = result?.ok_or_else(|| {
                    WikiError::new(
                        ErrorCode::IndexCorrupt,
                        "enumerated embedding document missing from pinned catalog",
                    )
                })?;
                // Explicitly unauthenticated domain, never a selected read proof.
                let fingerprint = Blake3Hash::digest(crate::graph::packet::canonical_json(&(
                    "lwiki-normalized-document-cached-discovery-v1",
                    self.reader.vault_id(),
                    &self.reader.snapshot().parser_fingerprint,
                    &document.hash,
                ))?);
                self.owner =
                    render_owner(&document, self.settings, fingerprint, self.budget)?.into_iter();
                continue;
            }
            if self.last_page {
                return Ok(None);
            }
            let before = self.reader.usage();
            let result = self
                .reader
                .embedding_document_paths(self.after.as_ref(), 128);
            self.budget.source_work(before, self.reader.usage());
            let paths = result?;
            self.last_page = paths.len() < 128;
            self.after = paths.last().cloned();
            self.paths = paths.into();
        }
    }
}
impl Iterator for CachedUnitIter<'_> {
    type Item = Result<RenderedUnit>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        match self.next_unit() {
            Ok(Some(unit)) => Some(Ok(unit)),
            Ok(None) => {
                self.failed = true;
                None
            }
            Err(error) => {
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}

pub(crate) struct AuthenticatedDocument {
    pub units: Vec<RenderedUnit>,
    /// Caller retains and rechecks this again at emission/commit boundary.
    pub proof: SelectedDocuments,
}
impl AuthenticatedDocument {
    pub(crate) fn read_preconditions(&self) -> Vec<ReadDependency> {
        self.proof.read_preconditions()
    }
    /// Includes the helper's final recheck; subsequent rechecks remain visible.
    pub(crate) fn verification_work(&self) -> (usize, usize, usize) {
        self.proof.meter().work()
    }
    pub(crate) fn bind_cached_unit(&self, cached: &RenderedUnit) -> Result<RenderedUnit> {
        let current = self
            .units
            .iter()
            .find(|unit| unit.unit_id == cached.unit_id)
            .ok_or_else(|| {
                WikiError::new(
                    ErrorCode::FreshnessConflict,
                    "cached winner missing from authenticated owner render",
                )
            })?;
        render::require_selected_unit_agreement(cached, current)?;
        let mut rebound = cached.clone();
        rebound.dependency_fingerprint = current.dependency_fingerprint.clone();
        Ok(rebound)
    }
}
/// Authenticate precisely one owner. Caller aggregates actual verification work
/// and passes the remaining byte/file/entry allowance on each subsequent call.
/// The shared deadline is never restarted, including inside selected verification.
pub(crate) fn materialize_owner(
    catalog: &Catalog,
    reader: &QuerySnapshot,
    path: &VaultRelativePath,
    settings: &EmbeddingSettings,
    verification_budget: &VerificationBudget,
    budget: &UnitBudget,
) -> Result<AuthenticatedDocument> {
    settings.validate()?;
    let remaining = budget.remaining_ms()?;
    let mut verification_budget = verification_budget.clone();
    verification_budget.max_elapsed_ms = verification_budget.max_elapsed_ms.min(remaining);
    let before = reader.usage();
    let result = selected_documents::authenticate(
        catalog,
        reader,
        std::slice::from_ref(path),
        &verification_budget,
    );
    budget.source_work(before, reader.usage());
    let mut proof = result?;
    if proof.documents.len() != 1 {
        return Err(WikiError::new(
            ErrorCode::FreshnessConflict,
            "embedding membership requires exactly one selected owner",
        ));
    }
    let document = proof.documents.get(path).ok_or_else(|| {
        WikiError::new(
            ErrorCode::FreshnessConflict,
            "authenticated embedding owner missing",
        )
    })?;
    let units = render_owner(
        document,
        settings,
        proof.embedding_dependency_fingerprint.clone(),
        budget,
    )?;
    proof.recheck(catalog, reader)?;
    budget.check()?;
    Ok(AuthenticatedDocument { units, proof })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        catalog::query_types::QueryReadLimits,
        sources::{CaptureRequest, ExtractionInput, SourceOrigin, SourcePlan, SourceStore},
        vault::{VaultFs, VaultRoot, WriterPermit},
    };
    use std::fs;

    fn path(s: &str) -> VaultRelativePath {
        VaultRelativePath::new(s).unwrap()
    }
    fn code<T>(result: Result<T>) -> ErrorCode {
        match result {
            Err(error) => error.code,
            Ok(_) => panic!("expected failure"),
        }
    }
    fn capture() -> CaptureRequest {
        CaptureRequest {
            title: "Captured café 東京".into(),
            origin_kind: SourceOrigin::LocalFile,
            origin: "fixture.txt".into(),
            original: "# Café 東京\n\nExact résumé evidence.\n\n"
                .repeat(20)
                .into_bytes(),
            extraction: ExtractionInput::Utf8Preserve,
            media_type: None,
        }
    }
    fn apply(root: &std::path::Path, plan: SourcePlan) {
        if let Some(draft) = plan.draft {
            for op in draft.operations {
                let target = root.join(op.target.as_str());
                fs::create_dir_all(target.parent().unwrap()).unwrap();
                match op.proposed {
                    Some(bytes) => fs::write(target, bytes).unwrap(),
                    None => fs::remove_file(target).unwrap(),
                }
            }
        }
    }
    fn fixture() -> (tempfile::TempDir, Catalog, VaultRelativePath, RecordId) {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("WIKI.md"),
            "---\nwiki_schema: '1'\nwiki_id: vault_units\nwiki_kind: vault\ntitle: Units\n---\n",
        )
        .unwrap();
        fs::write(temp.path().join("page.md"), format!("---\nwiki_schema: '1'\nwiki_id: page_units\nwiki_kind: page\ntitle: Authored café 東京\nwiki_status: reviewed\nwiki_depends_on_ids: [assertion_units]\n---\n{}", "# Café 東京\n\nSelected exact résumé.\n\n".repeat(20))).unwrap();
        fs::write(
            temp.path().join("plain.md"),
            "# Unmanaged\n\nExact unmanaged bytes.\n",
        )
        .unwrap();
        fs::write(temp.path().join("entity.md"), "---\nwiki_schema: '1'\nwiki_id: entity_units\nwiki_kind: entity\ntitle: Entity\nwiki_status: active\nwiki_entity_type: component\n---\n").unwrap();
        fs::write(temp.path().join("run.md"), "---\nwiki_schema: '1'\nwiki_id: run_units\nwiki_kind: run\ntitle: Run\nwiki_status: planned\nwiki_created_at: '2026-10-06T00:00:00Z'\n---\n").unwrap();
        fs::write(temp.path().join("event.md"), "---\nwiki_schema: '1'\nwiki_id: event_units\nwiki_kind: run_event\ntitle: Event\nwiki_run_id: run_units\nwiki_sequence: 1\nwiki_event_type: started\nwiki_occurred_at: '2026-10-06T00:00:00Z'\n---\n").unwrap();
        let fs = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let plan = SourceStore::new(fs.clone())
            .plan_capture(capture())
            .unwrap();
        let source_id = plan.source_id.clone();
        // A direct dependency on the unsupported Entity description would make
        // this Page stale. The accepted assertion uses its Current identity and
        // exact evidence from the already captured fixture instead.
        fs::write(temp.path().join("support.md"), "---\nwiki_schema: '1'\nwiki_id: assertion_units\nwiki_kind: assertion\ntitle: Supported identity fixture\nwiki_status: accepted\nwiki_subject_id: entity_units\nwiki_object_id: entity_units\nwiki_predicate: uses\n---\nSupported fixture proposition.\n").unwrap();
        let content = capture().original;
        let mut evidence = format!("---\nwiki_schema: '1'\nwiki_id: evidence_units\nwiki_kind: evidence\ntitle: Exact fixture support\nwiki_status: active\nwiki_assertion_id: assertion_units\nwiki_source_id: '{}'\nwiki_source_revision: '{}'\nwiki_stance: supports\nwiki_locator_kind: utf8-bytes\nwiki_span_start: 0\nwiki_span_end: {}\nwiki_quote_hash: {}\n---\n", plan.source_id, plan.revision_id, content.len(), Blake3Hash::digest(&content)).into_bytes();
        evidence
            .extend(crate::sources::evidence::exact_quote_body(&content, "\n", "Support").unwrap());
        fs::write(temp.path().join("evidence.md"), evidence).unwrap();
        let captured_path = plan
            .draft
            .as_ref()
            .unwrap()
            .operations
            .iter()
            .find(|op| op.target.as_str().ends_with("content.md"))
            .unwrap()
            .target
            .clone();
        apply(temp.path(), plan);
        let catalog = Catalog::new(fs, RecordId::new("vault_units").unwrap());
        (temp, catalog, captured_path, source_id)
    }
    fn publish(catalog: &Catalog, rebuild: bool) {
        let writer = WriterPermit::acquire(catalog.fs().root(), Duration::from_secs(1)).unwrap();
        if rebuild {
            catalog.rebuild_normalized(&writer).unwrap();
        } else {
            catalog.sync_normalized(&writer).unwrap();
        }
    }
    fn settings() -> EmbeddingSettings {
        EmbeddingSettings {
            max_input_bytes: 256,
            quality_target_bytes: Some(192),
            ..Default::default()
        }
    }
    fn budget() -> UnitBudget {
        UnitBudget::new(UnitLimits::default()).unwrap()
    }
    fn owner(
        catalog: &Catalog,
        reader: &QuerySnapshot,
        path: &VaultRelativePath,
        settings: &EmbeddingSettings,
    ) -> AuthenticatedDocument {
        materialize_owner(
            catalog,
            reader,
            path,
            settings,
            &VerificationBudget::default(),
            &mut budget(),
        )
        .unwrap()
    }
    #[test]
    fn exact_legacy_and_normalized_authored_and_captured_units_agree() {
        let (_temp, catalog, captured, _) = fixture();
        let settings = settings();
        let paths = [path("page.md"), captured];
        let legacy = {
            let writer =
                WriterPermit::acquire(catalog.fs().root(), Duration::from_secs(1)).unwrap();
            catalog.sync(&writer).unwrap();
            let reader = catalog.index_snapshot().unwrap();
            paths
                .iter()
                .map(|path| {
                    let row = reader
                        .projection()
                        .documents
                        .iter()
                        .find(|row| &row.path == path)
                        .unwrap();
                    render::render_document(&reader, row, &settings).unwrap()
                })
                .collect::<Vec<_>>()
        };
        publish(&catalog, true);
        let reader = catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap();
        for (path, legacy) in paths.iter().zip(legacy) {
            let selected = owner(&catalog, &reader, path, &settings);
            assert!(selected.units.len() > 1);
            assert_eq!(selected.units.len(), legacy.len());
            for (old, current) in legacy.iter().zip(&selected.units) {
                render::require_selected_unit_agreement(old, current).unwrap();
                assert!(current.utf8.len() <= 192);
                assert_eq!(
                    Blake3Hash::digest(current.utf8.as_bytes()),
                    current.input_hash
                );
            }
            assert!(!selected.read_preconditions().is_empty());
            assert!(selected.verification_work().0 > 0);
            assert!(selected.verification_work().1 > 1);
        }
    }
    #[test]
    fn scalar_pages_and_cached_iterator_exclude_operations_graph_and_withdrawn_capture() {
        let (temp, catalog, captured, source) = fixture();
        publish(&catalog, true);
        let reader = catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap();
        let mut after = None;
        let mut paths = Vec::new();
        loop {
            let page = reader.embedding_document_paths(after.as_ref(), 1).unwrap();
            if page.is_empty() {
                break;
            }
            after = page.last().cloned();
            paths.extend(page);
        }
        assert!(paths.contains(&path("page.md")) && paths.contains(&captured));
        for excluded in ["WIKI.md", "entity.md", "run.md", "event.md"] {
            assert!(!paths.contains(&path(excluded)));
            assert_eq!(
                code(materialize_owner(
                    &catalog,
                    &reader,
                    &path(excluded),
                    &settings(),
                    &VerificationBudget::default(),
                    &mut budget()
                )),
                ErrorCode::CapabilityUnavailable
            );
        }
        drop(reader);
        apply(
            temp.path(),
            SourceStore::new(catalog.fs().clone())
                .plan_withdraw(&source, "fixture withdrawal")
                .unwrap(),
        );
        publish(&catalog, false);
        let reader = catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap();
        let settings = settings();
        let factory = CachedDocumentUnits::new(&reader, &settings).unwrap();
        let units = factory
            .replay(&mut budget())
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert!(units.iter().all(|unit| unit.owner != captured));
        assert_eq!(
            code(materialize_owner(
                &catalog,
                &reader,
                &captured,
                &settings,
                &VerificationBudget::default(),
                &mut budget()
            )),
            ErrorCode::CapabilityUnavailable
        );
    }
    #[test]
    fn membership_survives_unrelated_run_publication_and_rebuild_but_binds_owner_edits() {
        let (temp, catalog, _, _) = fixture();
        publish(&catalog, true);
        let reader = catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap();
        let before = owner(&catalog, &reader, &path("page.md"), &settings());
        let stable = before.proof.embedding_dependency_fingerprint.clone();
        let read = before.proof.fingerprint.clone();
        drop(reader);
        let run = fs::read_to_string(temp.path().join("run.md")).unwrap();
        fs::write(
            temp.path().join("run.md"),
            run.replace("title: Run", "title: Unrelated Run"),
        )
        .unwrap();
        publish(&catalog, false);
        let reader = catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap();
        let after = owner(&catalog, &reader, &path("page.md"), &settings());
        assert_eq!(stable, after.proof.embedding_dependency_fingerprint);
        assert_ne!(read, after.proof.fingerprint);
        drop(reader);
        publish(&catalog, true);
        let reader = catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap();
        assert_eq!(
            stable,
            owner(&catalog, &reader, &path("page.md"), &settings())
                .proof
                .embedding_dependency_fingerprint
        );
        drop(reader);
        let entity = fs::read_to_string(temp.path().join("entity.md")).unwrap();
        fs::write(
            temp.path().join("entity.md"),
            entity.replace("title: Entity", "title: Changed entity"),
        )
        .unwrap();
        publish(&catalog, false);
        let reader = catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap();
        let dependency_edit = owner(&catalog, &reader, &path("page.md"), &settings());
        assert_ne!(
            stable,
            dependency_edit.proof.embedding_dependency_fingerprint
        );
        for (old, current) in before.units.iter().zip(&dependency_edit.units) {
            render::require_selected_unit_agreement(old, current).unwrap();
        }
        drop(reader);
        let page = fs::read_to_string(temp.path().join("page.md")).unwrap();
        fs::write(
            temp.path().join("page.md"),
            page.replace("Selected exact", "Selected other"),
        )
        .unwrap();
        publish(&catalog, false);
        let reader = catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap();
        assert_ne!(
            stable,
            owner(&catalog, &reader, &path("page.md"), &settings())
                .proof
                .embedding_dependency_fingerprint
        );
    }
    #[test]
    fn cached_discovery_cannot_authenticate_payload_edits_or_forged_winners() {
        let (temp, catalog, captured, _) = fixture();
        publish(&catalog, true);
        let reader = catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap();
        let settings = settings();
        let factory = CachedDocumentUnits::new(&reader, &settings).unwrap();
        let units = factory
            .replay(&mut budget())
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let cached = units.iter().find(|unit| unit.owner == captured).unwrap();
        let mut selected = owner(&catalog, &reader, &captured, &settings);
        assert_ne!(
            cached.dependency_fingerprint,
            selected.units[0].dependency_fingerprint
        );
        let bound = selected.bind_cached_unit(cached).unwrap();
        assert_eq!(bound, selected.units[0]);
        for field in 0..7 {
            let mut forged = cached.clone();
            match field {
                0 => forged.utf8.push('!'),
                1 => forged.input_hash = Blake3Hash::digest(b"forged input"),
                2 => forged.source_hash = Blake3Hash::digest(b"forged source"),
                3 => forged.source_span = None,
                4 => forged.owner = path("plain.md"),
                5 => forged.target_id = None,
                _ => forged.target = render::TargetKind::Entity,
            }
            assert_eq!(
                code(selected.bind_cached_unit(&forged)),
                ErrorCode::FreshnessConflict
            );
        }
        let raw = fs::read_to_string(temp.path().join(captured.as_str())).unwrap();
        let changed = raw.replace("Exact résumé", "Other résumé");
        assert_eq!(raw.len(), changed.len());
        fs::write(temp.path().join(captured.as_str()), changed).unwrap();
        assert_eq!(
            code(selected.proof.recheck(&catalog, &reader)),
            ErrorCode::FreshnessConflict
        );
        assert_eq!(
            code(materialize_owner(
                &catalog,
                &reader,
                &captured,
                &settings,
                &VerificationBudget::default(),
                &mut budget()
            )),
            ErrorCode::FreshnessConflict
        );
        // Cache discovery still returns old bytes, making its lack of authority observable.
        let cached_again = factory
            .replay(&mut budget())
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(units, cached_again);
    }
    #[test]
    fn selected_read_guards_bind_dependencies_and_meter_final_rechecks() {
        let (temp, catalog, _, _) = fixture();
        publish(&catalog, true);
        let reader = catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap();
        let mut selected = owner(&catalog, &reader, &path("page.md"), &settings());
        let guards = selected.read_preconditions();
        assert!(guards.iter().any(|guard| guard.path == path("entity.md")));
        let prior = selected.verification_work();
        selected.proof.recheck(&catalog, &reader).unwrap();
        assert!(selected.verification_work().0 > prior.0);
        assert!(selected.verification_work().1 > prior.1);
        assert!(selected.verification_work().2 > prior.2);
        let raw = fs::read_to_string(temp.path().join("entity.md")).unwrap();
        let changed = raw.replace("title: Entity", "title: Edited");
        assert_eq!(raw.len(), changed.len());
        // Payload authenticity is required regardless of indexed discovery.
        fs::write(temp.path().join("entity.md"), changed).unwrap();
        assert_eq!(
            code(selected.proof.recheck(&catalog, &reader)),
            ErrorCode::FreshnessConflict
        );
        assert_eq!(guards, selected.read_preconditions());
    }
    #[test]
    fn source_only_title_reuses_input_while_actual_page_header_edit_changes_it() {
        let (temp, catalog, captured, source) = fixture();
        publish(&catalog, true);
        let settings = settings();
        let reader = catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap();
        let before = owner(&catalog, &reader, &captured, &settings);
        let page_before = owner(&catalog, &reader, &path("page.md"), &settings);
        drop(reader);
        let plan = SourceStore::new(catalog.fs().clone())
            .plan_refresh_with_title(&source, capture(), Some("Mutable Source title"))
            .unwrap();
        assert!(plan.reused);
        apply(temp.path(), plan);
        publish(&catalog, false);
        let reader = catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap();
        let after = owner(&catalog, &reader, &captured, &settings);
        assert_ne!(
            before.proof.embedding_dependency_fingerprint,
            after.proof.embedding_dependency_fingerprint
        );
        assert_eq!(before.units.len(), after.units.len());
        for (old, new) in before.units.iter().zip(&after.units) {
            render::require_selected_unit_agreement(old, new).unwrap();
        }
        drop(reader);
        let page = fs::read_to_string(temp.path().join("page.md")).unwrap();
        fs::write(
            temp.path().join("page.md"),
            page.replace("title: Authored café 東京", "title: Changed café 東京"),
        )
        .unwrap();
        publish(&catalog, false);
        let reader = catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap();
        let page_after = owner(&catalog, &reader, &path("page.md"), &settings);
        assert_ne!(
            page_before.units[0].input_hash,
            page_after.units[0].input_hash
        );
    }
    #[test]
    fn scalar_decode_render_replay_and_deadline_budgets_fail_without_reset() {
        let (_temp, catalog, _, _) = fixture();
        publish(&catalog, true);
        let reader = catalog
            .cached_query_snapshot(QueryReadLimits {
                max_rows: 1,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            code(reader.embedding_document_paths(None, 128)),
            ErrorCode::BudgetExceeded
        );
        assert_eq!(reader.usage().rows, 1);
        assert_eq!(
            code(reader.embedding_document_paths(None, 129)),
            ErrorCode::Usage
        );
        let reader = catalog
            .cached_query_snapshot(QueryReadLimits {
                max_row_bytes: 32,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            code(reader.document(&path("page.md"))),
            ErrorCode::BudgetExceeded
        );
        let settings = settings();
        let factory = CachedDocumentUnits::new(&reader, &settings).unwrap();
        assert_eq!(
            code(
                factory
                    .replay(&mut budget())
                    .unwrap()
                    .collect::<Result<Vec<_>>>()
            ),
            ErrorCode::BudgetExceeded
        );
        let replay_reader = catalog
            .cached_query_snapshot(QueryReadLimits {
                max_rows: 6,
                ..Default::default()
            })
            .unwrap();
        let replay_factory = CachedDocumentUnits::new(&replay_reader, &settings).unwrap();
        let mut replay_budget = budget();
        replay_factory
            .replay(&mut replay_budget)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(replay_reader.usage().rows, 6);
        assert_eq!(
            code(
                replay_factory
                    .replay(&mut replay_budget)
                    .unwrap()
                    .collect::<Result<Vec<_>>>()
            ),
            ErrorCode::BudgetExceeded
        );
        assert_eq!(replay_reader.usage().rows, 6);
        let reader = catalog
            .cached_query_snapshot(QueryReadLimits::default())
            .unwrap();
        let factory = CachedDocumentUnits::new(&reader, &settings).unwrap();
        let mut render_budget = UnitBudget::new(UnitLimits {
            max_render_bytes: 1,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            code(
                factory
                    .replay(&mut render_budget)
                    .unwrap()
                    .collect::<Result<Vec<_>>>()
            ),
            ErrorCode::BudgetExceeded
        );
        let mut shared = budget();
        let first = factory
            .replay(&mut shared)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let work = shared.usage();
        let source = reader.usage();
        shared.limits.max_units = first.len();
        assert_eq!(
            code(
                factory
                    .replay(&mut shared)
                    .unwrap()
                    .collect::<Result<Vec<_>>>()
            ),
            ErrorCode::BudgetExceeded
        );
        assert!(reader.usage().rows > source.rows);
        assert!(shared.usage().source_rows > work.source_rows);
        assert_eq!(shared.usage().units, work.units);
        let mut owner_budget = UnitBudget::new(UnitLimits {
            max_owner_render_bytes: 1,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            code(materialize_owner(
                &catalog,
                &reader,
                &path("page.md"),
                &settings,
                &VerificationBudget::default(),
                &mut owner_budget
            )),
            ErrorCode::BudgetExceeded
        );
        assert_eq!(
            code(UnitBudget::with_deadline(
                UnitLimits::default(),
                Instant::now()
            )),
            ErrorCode::BudgetExceeded
        );
    }
}

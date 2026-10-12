//! Published document discovery with a bounded, selected canonical proof.
use super::{
    context,
    context_types::*,
    lexical, selected_documents,
    verification::{Meter, seal},
};
use crate::{
    catalog::{
        Catalog, CatalogDiagnostic, DocumentRow, RecordRow, SnapshotVerification,
        query::QuerySnapshot,
        query_types::{QueryCatalog, QueryReadLimits},
    },
    domain::*,
};
use rusqlite::{Connection, Row};
use std::collections::BTreeSet;

/// Shared packing may only obtain authenticated objects, never fall back to
/// unselected cached records or decode more documents through this adapter.
pub(super) struct SelectedCatalog<'a> {
    pub(super) reader: &'a QuerySnapshot,
    pub(super) proof: &'a selected_documents::SelectedDocuments,
}
impl QueryCatalog for SelectedCatalog<'_> {
    fn query_deadline(&self) -> Option<std::time::Instant> {
        self.reader.query_deadline()
    }

    fn normalized_layout(&self) -> bool {
        self.reader.normalized_layout()
    }
    fn current_observation_layout(&self) -> bool {
        self.reader.current_observation_layout()
    }
    fn publication_id(&self) -> Option<&str> {
        self.reader.publication_id()
    }
    fn connection(&self) -> &Connection {
        self.reader.connection()
    }
    fn check_query_budget(&self) -> Result<()> {
        self.reader.check_query_budget()
    }
    fn snapshot(&self) -> &ReadSnapshot {
        self.reader.snapshot()
    }
    fn vault_id(&self) -> &RecordId {
        self.reader.vault_id()
    }
    fn verification(&self) -> &SnapshotVerification {
        self.reader.verification()
    }
    fn record(&self, id: &RecordId) -> Result<Option<RecordRow>> {
        Ok(self.proof.records.get(id).cloned())
    }
    fn document(&self, path: &VaultRelativePath) -> Result<Option<DocumentRow>> {
        Ok(self.proof.documents.get(path).cloned())
    }
    fn diagnostics(&self, _: &BTreeSet<VaultRelativePath>) -> Result<Vec<CatalogDiagnostic>> {
        Err(WikiError::new(
            ErrorCode::FreshnessConflict,
            "selected document proof has no uncaptured diagnostics",
        ))
    }
    fn dependency_fingerprint(&self) -> Result<Blake3Hash> {
        if self.reader.is_current_observation() {
            self.reader.dependency_fingerprint()
        } else {
            Ok(self.proof.fingerprint.clone())
        }
    }
    fn query_scope(&self) -> &'static str {
        if self.reader.is_current_observation() {
            "current_observation"
        } else {
            "indexed_documents"
        }
    }
    fn decode_document(&self, _: &Row<'_>, _: usize) -> Result<DocumentRow> {
        Err(WikiError::new(
            ErrorCode::FreshnessConflict,
            "selected document proof cannot decode additional documents",
        ))
    }
}

pub(crate) fn verification(reader: &QuerySnapshot) -> Result<SnapshotVerification> {
    let usage = reader.usage();
    Ok(SnapshotVerification::IndexedEvidence {
        verified_at: crate::sources::revision::timestamp()?,
        discovery_generation: reader.snapshot().generation,
        evidence_domain: "selected_documents".into(),
        global_membership_verified: false,
        catalog_rows_decoded: usage.rows,
        catalog_bytes_decoded: usage.bytes,
        pending_operation_at_start: reader.pending_operation_at_start(),
    })
}

pub(crate) fn context(
    catalog: &Catalog,
    query: &str,
    request: &ContextRequest,
    options: &ContextOptions,
) -> Result<ContextResult> {
    let mut meter = Meter::new(&request.verification_budget);
    let request = context::validate_request(query, request)?;
    context::validate_selection_action(&request, &options.selection)?;
    context::validate_source_relations(&request, options)?;
    if request.scope != ContextScope::IndexedDocuments
        || !matches!(
            request.documents.mode,
            super::SearchMode::Literal | super::SearchMode::Lexical
        )
        || catalog.operation_state()?.is_none()
    {
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "literal/lexical indexed-documents requires a selected normalized index; semantic modes use their selected vector coordinator",
        ));
    }
    meter.check()?;
    catalog.guard_query()?;
    meter.check()?;
    let reader = catalog.cached_query_snapshot(QueryReadLimits {
        max_elapsed_ms: meter.remaining_ms(),
        ..QueryReadLimits::default()
    })?;
    context_with_held_reader(catalog, &reader, query, &request, options, &mut meter)
}

/// Reuse document packing against the exact reader retained by the ordinary
/// normalized lookup coordinator. Explicit indexed callers keep their scope.
pub(crate) fn context_with_reader(
    catalog: &Catalog,
    reader: &QuerySnapshot,
    query: &str,
    request: &ContextRequest,
    options: &ContextOptions,
) -> Result<ContextResult> {
    let mut meter = Meter::new(&request.verification_budget);
    context_with_held_reader(catalog, reader, query, request, options, &mut meter)
}

fn context_with_held_reader(
    catalog: &Catalog,
    reader: &QuerySnapshot,
    query: &str,
    request: &ContextRequest,
    options: &ContextOptions,
    meter: &mut Meter,
) -> Result<ContextResult> {
    let request = context::validate_request(query, request)?;
    context::validate_selection_action(&request, &options.selection)?;
    context::validate_source_relations(&request, options)?;
    let current = reader.is_current_observation();
    if !reader.normalized_layout()
        || !matches!(
            request.documents.mode,
            super::SearchMode::Literal | super::SearchMode::Lexical
        )
        || (current
            && (request.scope != ContextScope::Current
                || request.target != ContextTarget::Documents
                || request.graph.is_some()
                || !matches!(
                    options.selection,
                    super::context_selection_packet::SelectionAction::Automatic
                )
                || options.experimental_source_relations
                || options.experimental_hybrid_lexical_evidence))
        || (!current && request.scope != ContextScope::IndexedDocuments)
    {
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "held normalized context requires current local documents or published indexed-documents scope",
        ));
    }
    meter.check()?;
    reader.verify_operations(catalog)?;
    if matches!(
        options.selection,
        super::context_selection_packet::SelectionAction::PrepareOriginals(_)
            | super::context_selection_packet::SelectionAction::ApplyOriginals { .. }
            | super::context_selection_packet::SelectionAction::PrepareOriginalsAuto { .. }
            | super::context_selection_packet::SelectionAction::ApplyOriginalsAuto { .. }
    ) {
        return super::context_original_selection::context(
            catalog, reader, meter, query, &request, options,
        );
    }
    let mut hits = lexical::search_context_catalog(
        reader,
        query,
        &request.documents,
        request.documents.filters.include_historical,
    )?;
    meter.check()?;
    let paths = hits
        .hits
        .iter()
        .map(|hit| hit.locator.path.clone())
        .collect::<Vec<_>>();
    let mut budget = request.verification_budget.clone();
    budget.max_elapsed_ms = meter.remaining_ms();
    if current {
        budget = super::verification::current_proof_budget(reader, &budget)?;
    }
    let mut proof = selected_documents::authenticate(catalog, reader, &paths, &budget)?;
    let mut relation_omissions = Vec::new();
    let mut signals = ContextSelectionSignals::default();
    if options.experimental_source_relations {
        let loaded = proof.load_source_relations(catalog, reader, &paths)?;
        signals.relations = loaded.annotations;
        signals.relation_deadline = Some(loaded.deadline);
        signals.warnings = loaded.warnings;
        relation_omissions = loaded.omissions;
    }
    // Candidates belong to the pinned generation; assembly is additionally bound
    // to the exact selected canonical dependency set authenticated above.
    hits.dependency_fingerprint = if current {
        reader.dependency_fingerprint()?
    } else {
        proof.fingerprint.clone()
    };
    let selected = SelectedCatalog {
        reader,
        proof: &proof,
    };
    let mut draft = context::assemble_bounded_documents_with_signals(
        &selected,
        &request,
        &hits,
        query,
        &signals,
        &options.selection,
    )?;
    draft.omissions.extend(relation_omissions);
    reader.check_query_budget()?;
    if !current {
        draft.warnings.push("Discovery uses the published generation; selected document dependencies are verified. Global membership, identity uniqueness, completeness and unselected freshness are not verified. Use index sync to discover external edits.".into());
    }
    if let Some(fault) = &options.fault {
        fault.check(ContextCheckpoint::BeforeFinalVerification { attempt: 0 })?;
    }
    proof.recheck(catalog, reader)?;
    reader.check_query_budget()?;
    let verification = if current {
        super::verification::verify_current_observation(catalog, reader, &proof)?
    } else {
        verification(reader)?
    };
    meter.check()?;
    Ok(seal(draft, verification, proof.meter()))
}

#[cfg(test)]
#[path = "indexed_documents_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "indexed_documents_dense_experiment.rs"]
mod dense_experiment;

#[cfg(test)]
mod current_layout_regression {
    use super::*;
    use crate::{
        catalog::transient_read::CurrentQueryAttempt,
        vault::{VaultFs, VaultRoot, WriterPermit},
    };
    use std::{fs, time::Duration};

    #[test]
    fn selected_adapter_retains_private_current_layout_for_lexical_preflight() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("WIKI.md"), "---\nwiki_schema: '1'\nwiki_id: vault_current_adapter\nwiki_kind: vault\ntitle: Adapter\n---\n").unwrap();
        let catalog = Catalog::new(
            VaultFs::new(VaultRoot::explicit(temp.path()).unwrap()),
            RecordId::new("vault_current_adapter").unwrap(),
        );
        let writer = WriterPermit::acquire(catalog.fs().root(), Duration::from_secs(1)).unwrap();
        catalog.rebuild_normalized(&writer).unwrap();
        drop(writer);
        let CurrentQueryAttempt::Ready(reader) = catalog
            .current_query_snapshot(QueryReadLimits::default())
            .unwrap()
        else {
            panic!("small fixture must fit")
        };
        let proof =
            selected_documents::authenticate(&catalog, &reader, &[], &Default::default()).unwrap();
        let selected = SelectedCatalog {
            reader: &reader,
            proof: &proof,
        };
        assert!(selected.current_observation_layout());
        // An empty discovery page is deliberate: the closed selected adapter
        // forbids hydrating additional documents, but must pass layout checks.
        assert!(
            super::super::lexical::search_catalog(
                &selected,
                "absentzirconium",
                &Default::default()
            )
            .unwrap()
            .hits
            .is_empty()
        );
        reader
            .connection()
            .execute_batch("PRAGMA query_only=OFF; DROP INDEX document_candidate_metadata;")
            .unwrap();
        let error = super::super::lexical::search_catalog(
            &selected,
            "absentzirconium",
            &Default::default(),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::CapabilityUnavailable);
    }
}

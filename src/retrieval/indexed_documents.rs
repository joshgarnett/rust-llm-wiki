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
    fn normalized_layout(&self) -> bool {
        self.reader.normalized_layout()
    }
    fn publication_id(&self) -> Option<&str> {
        self.reader.publication_id()
    }
    fn connection(&self) -> &Connection {
        self.reader.connection()
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
        Ok(self.proof.fingerprint.clone())
    }
    fn query_scope(&self) -> &'static str {
        "indexed_documents"
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
    let meter = Meter::new(&request.verification_budget);
    let request = context::validate_request(query, request)?;
    context::validate_selection_action(&request, &options.selection)?;
    if request.scope != ContextScope::IndexedDocuments
        || request.documents.mode != super::SearchMode::Lexical
        || catalog.operation_state()?.is_none()
    {
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "lexical indexed-documents requires a selected normalized index; semantic modes use their selected vector coordinator",
        ));
    }
    meter.check()?;
    catalog.guard_query()?;
    meter.check()?;
    let reader = catalog.cached_query_snapshot(QueryReadLimits {
        max_elapsed_ms: meter.remaining_ms(),
        ..QueryReadLimits::default()
    })?;
    let mut hits = lexical::search_context_catalog(&reader, query, &request.documents, false)?;
    meter.check()?;
    let paths = hits
        .hits
        .iter()
        .map(|hit| hit.locator.path.clone())
        .collect::<Vec<_>>();
    let mut budget = request.verification_budget.clone();
    budget.max_elapsed_ms = meter.remaining_ms();
    let mut proof = selected_documents::authenticate(catalog, &reader, &paths, &budget)?;
    // Candidates belong to the pinned generation; assembly is additionally bound
    // to the exact selected canonical dependency set authenticated above.
    hits.dependency_fingerprint = proof.fingerprint.clone();
    let selected = SelectedCatalog {
        reader: &reader,
        proof: &proof,
    };
    let mut draft = context::assemble_bounded_documents_with_selection_for_query(
        &selected,
        &request,
        &hits,
        query,
        &options.selection,
    )?;
    draft.warnings.push("Discovery uses the published generation; selected document dependencies are verified. Global membership, identity uniqueness, completeness and unselected freshness are not verified. Use index sync to discover external edits.".into());
    if let Some(fault) = &options.fault {
        fault.check(ContextCheckpoint::BeforeFinalVerification { attempt: 0 })?;
    }
    proof.recheck(catalog, &reader)?;
    let verification = verification(&reader)?;
    meter.check()?;
    Ok(seal(draft, verification, proof.meter()))
}

#[cfg(test)]
#[path = "indexed_documents_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "indexed_documents_dense_experiment.rs"]
mod dense_experiment;

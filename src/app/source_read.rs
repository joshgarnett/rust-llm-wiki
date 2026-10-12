//! Direct source identity resolution using the ordinary writer-free selected proof.
use super::{MAX_INPUT_BYTES, OfflineApp, ReadOutcome, ReadSourceCitation};
use crate::{
    catalog::{
        Catalog, DocumentRow, RecordRow, SnapshotVerification,
        query_types::{QueryCatalog, QueryReadLimits},
        scan::{ReadScanBudget, ReadScanLimits},
        transient_read::CurrentQueryAttempt,
    },
    domain::{
        Blake3Hash, ByteSpan, CitationRef, Eligibility, ErrorCode, ReadSnapshot, RecordId,
        RecordKind, Result, SourceSpanRef, VaultRelativePath, WikiError,
    },
    retrieval::{VerificationBudget, selected_documents, verification},
    sources::SourceCaptureState,
};
use serde::Serialize;

#[derive(Debug, Clone)]
pub struct SourceReadRequest {
    pub source_id: RecordId,
    pub revision_id: Option<RecordId>,
    pub range: Option<ByteSpan>,
    pub from: Option<u64>,
    pub max_bytes: usize,
}
#[derive(Debug, Clone, Serialize)]
pub struct SourceReadOutcome {
    pub source_id: RecordId,
    pub revision_id: RecordId,
    pub title: String,
    pub capture_state: SourceCaptureState,
    pub read: ReadOutcome,
    pub snapshot: ReadSnapshot,
    pub verification: SnapshotVerification,
    pub persistent_generation: Option<u64>,
    pub current_observation: bool,
    /// A bounded current observation can fall back to selected verified saved discovery.
    pub current_observation_unavailable: Option<String>,
}

fn missing(message: impl Into<String>) -> WikiError {
    WikiError::new(ErrorCode::RecordNotFound, message)
}
fn selected_target(
    reader: &dyn QueryCatalog,
    request: &SourceReadRequest,
) -> Result<(RecordRow, RecordRow, DocumentRow)> {
    let source = reader
        .record(&request.source_id)?
        .ok_or_else(|| missing(format!("Source not found: {}", request.source_id)))?;
    if source.record.kind() != RecordKind::Source || source.record.id() != &request.source_id {
        return Err(WikiError::new(
            ErrorCode::SourceIntegrity,
            "source read requires a Source reference",
        ));
    }
    let revision_id = request.revision_id.clone().unwrap_or(RecordId::new(
        source
            .record
            .string("wiki_current_revision")
            .ok_or_else(|| missing("Source has no captured head"))?,
    )?);
    let revision = reader
        .record(&revision_id)?
        .ok_or_else(|| missing(format!("Captured revision not found: {revision_id}")))?;
    if revision.record.kind() != RecordKind::Revision
        || revision.record.string("wiki_source_id") != Some(request.source_id.as_str())
        || !source
            .record
            .field("wiki_revisions")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|values| {
                values
                    .iter()
                    .any(|id| id.as_str() == Some(revision_id.as_str()))
            })
    {
        return Err(WikiError::new(
            ErrorCode::SourceIntegrity,
            "requested revision does not belong to this Source's retained history",
        ));
    }
    let Some(relative) = revision.record.string("wiki_content_path") else {
        let mut error = WikiError::new(
            ErrorCode::CapabilityUnavailable,
            format!(
                "{} has no readable captured text; its original bytes and history are retained",
                source.record.title()
            ),
        );
        error.details = serde_json::json!({"source_id":request.source_id,"revision_id":revision_id,"capture_state":"unsupported"});
        error.hint = Some("Export the original as UTF-8 Markdown or plain text, then run lwiki source capture on that file to search and cite it.".into());
        return Err(error);
    };
    let parent = revision
        .path
        .as_str()
        .rsplit_once('/')
        .ok_or_else(|| missing("captured revision has no payload folder"))?
        .0;
    let path = VaultRelativePath::new(format!("{parent}/{relative}"))?;
    let document = reader.document(&path)?.ok_or_else(|| {
        missing("captured text is unavailable; check this Source and its retained revision")
    })?;
    if document.source_id.as_ref() != Some(&request.source_id)
        || document.owner_revision.as_ref() != Some(&revision_id)
    {
        return Err(WikiError::new(
            ErrorCode::SourceIntegrity,
            "captured text ownership differs from the selected Source",
        ));
    }
    Ok((source, revision, document))
}

fn format_source(
    source: &RecordRow,
    revision: &RecordRow,
    document: &DocumentRow,
    request: &SourceReadRequest,
    snapshot: &ReadSnapshot,
) -> Result<SourceReadOutcome> {
    let range =
        super::offline::requested_read_range(&document.raw_text, request.range, request.from)?;
    let start = usize::try_from(range.start())
        .map_err(|_| WikiError::invalid("source read range exceeds platform bounds"))?;
    let wanted = usize::try_from(range.end())
        .map_err(|_| WikiError::invalid("source read range exceeds platform bounds"))?;
    let end =
        super::offline::bounded_utf8_end(&document.raw_text, start, wanted, request.max_bytes)?;
    let emitted = ByteSpan::new(start as u64, end as u64)?;
    let body = document.raw_text[start..end].to_owned();
    let citation = if emitted.is_empty() {
        None
    } else {
        if !matches!(
            document.eligibility,
            Eligibility::Current | Eligibility::Historical | Eligibility::Withdrawn
        ) {
            return Err(WikiError::new(
                ErrorCode::SourceIntegrity,
                "captured text is not eligible for a source citation",
            ));
        }
        Some(ReadSourceCitation {
            citation: CitationRef::Source(SourceSpanRef {
                source_id: source.record.id().clone(),
                source_revision: revision.record.id().clone(),
                span: emitted,
                quote_hash: Blake3Hash::digest(body.as_bytes()),
            }),
            eligibility: document.eligibility,
        })
    };
    Ok(SourceReadOutcome {
        source_id: source.record.id().clone(),
        revision_id: revision.record.id().clone(),
        title: source.record.title().to_owned(),
        capture_state: if document.raw_text.is_empty() {
            SourceCaptureState::Empty
        } else {
            SourceCaptureState::Complete
        },
        read: ReadOutcome {
            path: document.path.clone(),
            hash: document.hash.clone(),
            record: None,
            metadata: None,
            diagnostics: vec![],
            body,
            range: emitted,
            truncated: end < wanted,
            continuation: if end < wanted {
                Some(ByteSpan::new(end as u64, wanted as u64)?)
            } else {
                None
            },
            source_citation: citation,
        },
        // Replaced by the caller only after the ordinary final proof passes.
        snapshot: snapshot.clone(),
        verification: SnapshotVerification::IndexSnapshot,
        persistent_generation: None,
        current_observation: false,
        current_observation_unavailable: None,
    })
}

impl OfflineApp {
    pub fn read_source(&self, request: SourceReadRequest) -> Result<SourceReadOutcome> {
        if request.max_bytes == 0 || request.max_bytes > MAX_INPUT_BYTES {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "source read limit must be 1..=16 MiB",
            ));
        }
        let catalog: Catalog = self.catalog();
        if catalog.operation_state()?.is_some() {
            catalog.guard_query()?;
            let (reader, fallback) =
                match catalog.current_query_snapshot(QueryReadLimits::default())? {
                    CurrentQueryAttempt::Ready(reader) => (*reader, None),
                    CurrentQueryAttempt::BoundedUnavailable(reason) => (
                        catalog.published_read_only_query_snapshot(QueryReadLimits::default())?,
                        Some(reason.message),
                    ),
                };
            let (source, revision, document) = selected_target(&reader, &request)?;
            // Include both selected envelopes even for an explicit historical
            // payload, so the title/head returned with it is also authenticated.
            let paths = [
                source.path.clone(),
                revision.path.clone(),
                document.path.clone(),
            ];
            let requested = VerificationBudget::default();
            let budget = if reader.is_current_observation() {
                verification::current_proof_budget(&reader, &requested)?
            } else {
                requested
            };
            let mut proof = selected_documents::authenticate(&catalog, &reader, &paths, &budget)?;
            proof.recheck(&catalog, &reader)?;
            for selected in [&source, &revision] {
                let authenticated = proof.records.get(selected.record.id()).ok_or_else(|| {
                    missing("Source or Revision envelope escaped its selected proof")
                })?;
                if authenticated.hash != selected.hash
                    || authenticated.path != selected.path
                    || authenticated.record != selected.record
                {
                    return Err(WikiError::new(
                        ErrorCode::FreshnessConflict,
                        "Source title or revision head differs from its authenticated proof",
                    ));
                }
            }
            let document = proof
                .documents
                .get(&document.path)
                .ok_or_else(|| missing("source text escaped its selected proof"))?;
            let mut outcome =
                format_source(&source, &revision, document, &request, reader.snapshot())?;
            outcome.verification = if reader.is_current_observation() {
                verification::verify_current_observation(&catalog, &reader, &proof)?
            } else {
                crate::retrieval::indexed_documents::verification(&reader)?
            };
            outcome.snapshot = reader.snapshot().clone();
            outcome.persistent_generation = reader.persistent_generation();
            outcome.current_observation = reader.is_current_observation();
            outcome.current_observation_unavailable = fallback;
            return Ok(outcome);
        }
        let budget = ReadScanBudget::new(ReadScanLimits::default())?;
        let reader = catalog.read_only_snapshot_with_budget(&budget)?;
        let (source, revision, document) = selected_target(&reader, &request)?;
        let mut outcome =
            format_source(&source, &revision, &document, &request, reader.snapshot())?;
        // The ordinary legacy snapshot includes canonical identity membership,
        // current heads, immutable ownership and exact captured content hashes.
        catalog.recheck_read_snapshot(&reader, &budget)?;
        outcome.snapshot = reader.snapshot().clone();
        outcome.verification = reader.verification().clone();
        outcome.persistent_generation = reader.persistent_generation();
        Ok(outcome)
    }
}

#[cfg(test)]
#[path = "source_read_tests.rs"]
mod tests;

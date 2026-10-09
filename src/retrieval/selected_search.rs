//! Published literal/lexical discovery with one bounded proof of the displayed document page.
use super::{
    ExcerptLabel, HitSet, QueryPlan, RetrievalReason, SearchExcerpt, SearchHit, SearchMode,
    context_types::VerificationBudget, indexed_documents, lexical, selected_documents,
    verification::Meter,
};
use crate::{
    catalog::{
        Catalog,
        query_types::{QueryCatalog, QueryReadLimits},
    },
    domain::*,
};
#[cfg(test)]
use serde::Serialize;
#[cfg(test)]
use std::time::Instant;

pub(super) const SCOPE_WARNING: &str = "Selected document dependencies are verified against the published discovery page. Global membership, identity uniqueness, completeness and unselected freshness are not verified.";

#[cfg(test)]
#[derive(Default, Debug, Serialize)]
pub(super) struct Stats {
    pub(super) elapsed_ns: u128,
    pub(super) catalog_rows: usize,
    pub(super) catalog_bytes: usize,
    // None means authentication did not return a meter, not zero work.
    pub(super) proof_work: Option<(usize, usize, usize)>,
}
fn conflict(message: &str) -> WikiError {
    WikiError::new(ErrorCode::FreshnessConflict, message)
}
fn serialization(error: serde_json::Error) -> WikiError {
    WikiError::new(ErrorCode::Internal, error.to_string())
}

fn bind_excerpt(excerpt: &mut SearchExcerpt, hit: &SearchHit, raw: &str) -> Result<()> {
    let start =
        usize::try_from(excerpt.span.start()).map_err(|_| conflict("excerpt start overflow"))?;
    let end = usize::try_from(excerpt.span.end()).map_err(|_| conflict("excerpt end overflow"))?;
    if raw.get(start..end) != Some(excerpt.text.as_str())
        || excerpt.matched_spans.iter().any(|span| {
            span.start() < excerpt.span.start()
                || span.end() > excerpt.span.end()
                || usize::try_from(span.start())
                    .ok()
                    .zip(usize::try_from(span.end()).ok())
                    .is_none_or(|(start, end)| raw.get(start..end).is_none())
        })
        || excerpt.citation.is_some()
        || excerpt.label != ExcerptLabel::NoteText
    {
        return Err(conflict(
            "displayed excerpt differs from authenticated UTF-8 bytes",
        ));
    }
    if !excerpt.span.is_empty()
        && matches!(
            hit.eligibility,
            Eligibility::Current | Eligibility::Historical | Eligibility::Withdrawn
        )
        && let (Some(source), Some(revision)) = (&hit.source_id, &hit.owner_revision)
    {
        excerpt.citation = Some(CitationRef::Source(SourceSpanRef {
            source_id: source.clone(),
            source_revision: revision.clone(),
            span: excerpt.span,
            quote_hash: Blake3Hash::digest(excerpt.text.as_bytes()),
        }));
        excerpt.label = ExcerptLabel::CapturedSource;
    }
    Ok(())
}

pub(super) fn bind_hit(
    hit: &mut SearchHit,
    proof: &selected_documents::SelectedDocuments,
    vault: &RecordId,
) -> Result<()> {
    let document = proof
        .documents
        .get(&hit.locator.path)
        .ok_or_else(|| conflict("displayed path escaped selected proof"))?;
    let record = document
        .record_id
        .as_ref()
        .and_then(|id| proof.records.get(id));
    let expected_record = record
        .map(|row| RecordRef {
            vault_id: vault.clone(),
            record_id: row.record.id().clone(),
            expected_kind: row.record.kind(),
        })
        .or_else(|| {
            document
                .owner_revision
                .as_ref()
                .and_then(|id| proof.records.get(id))
                .map(|row| RecordRef {
                    vault_id: vault.clone(),
                    record_id: row.record.id().clone(),
                    expected_kind: row.record.kind(),
                })
        });
    if hit.locator.path != document.path
        || hit.locator.observed_hash != document.hash
        || hit.locator.record != expected_record
        || hit.title != document.title
        || hit.kind != document.kind
        || hit.authored_status != record.and_then(|row| row.authored_status.clone())
        || hit.identity_eligibility != record.and_then(|row| row.identity_eligibility)
        || hit.eligibility != document.eligibility
        || hit.source_id != document.source_id
        || hit.owner_revision != document.owner_revision
    {
        return Err(conflict(
            "displayed locator or metadata differs from authenticated document",
        ));
    }
    // A current Entity identity can be navigable without a supported description.
    // Authenticate its record and keep that channel empty and uncited; body
    // eligibility never inherits authority from an Identity retrieval reason.
    if matches!(
        document.eligibility,
        Eligibility::Invalid | Eligibility::Unsupported
    ) && !(record.is_some_and(|row| {
        row.record.kind() == RecordKind::Entity
            && row.identity_eligibility == Some(Eligibility::Current)
    }) && hit.reasons.contains(&RetrievalReason::Identity)
        && document.source_id.is_none()
        && document.owner_revision.is_none()
        && hit.excerpt.span.is_empty()
        && hit.excerpt.text.is_empty()
        && hit.excerpt.matched_spans.is_empty()
        && hit.excerpt.citation.is_none()
        && hit.excerpt.label == ExcerptLabel::NoteText
        && hit.secondary_excerpts.is_empty())
    {
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "selected search cannot confer evidence authority on invalid or unsupported discovery",
        ));
    }
    let binding = hit.clone();
    bind_excerpt(&mut hit.excerpt, &binding, &document.raw_text)?;
    for excerpt in &mut hit.secondary_excerpts {
        bind_excerpt(excerpt, &binding, &document.raw_text)?;
    }
    Ok(())
}

/// Verify only the displayed page against its pinned published generation.
/// The caller supplies one finite whole-operation proof budget; this never synchronizes.
pub fn search(
    catalog: &Catalog,
    query: &str,
    plan: &QueryPlan,
    budget: &VerificationBudget,
) -> Result<HitSet> {
    #[cfg(test)]
    {
        measured_search(catalog, query, plan, budget, || Ok(())).0
    }
    #[cfg(not(test))]
    {
        coordinate(catalog, query, plan, budget, false)
    }
}

/// Ordinary CLI lexical discovery retains the same selected-page proof.
/// Context, hybrid and explicit lexical fallback keep the baseline entry above.
pub fn search_ordinary_lexical(
    catalog: &Catalog,
    query: &str,
    plan: &QueryPlan,
    budget: &VerificationBudget,
) -> Result<HitSet> {
    if plan.mode != SearchMode::Lexical {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "ordinary selected lexical discovery requires lexical mode",
        ));
    }
    #[cfg(test)]
    {
        coordinate(
            catalog,
            query,
            plan,
            budget,
            true,
            || Ok(()),
            &mut Stats::default(),
        )
    }
    #[cfg(not(test))]
    {
        coordinate(catalog, query, plan, budget, true)
    }
}

#[cfg(test)]
pub(super) fn measured_search<F: FnOnce() -> Result<()>>(
    catalog: &Catalog,
    query: &str,
    plan: &QueryPlan,
    budget: &VerificationBudget,
    before_final: F,
) -> (Result<HitSet>, Stats) {
    let start = Instant::now();
    let mut stats = Stats::default();
    let result = coordinate(
        catalog,
        query,
        plan,
        budget,
        false,
        before_final,
        &mut stats,
    );
    stats.elapsed_ns = start.elapsed().as_nanos();
    (result, stats)
}

fn coordinate(
    catalog: &Catalog,
    query: &str,
    plan: &QueryPlan,
    budget: &VerificationBudget,
    ordinary_discovery: bool,
    #[cfg(test)] before_final: impl FnOnce() -> Result<()>,
    #[cfg(test)] stats: &mut Stats,
) -> Result<HitSet> {
    let meter = Meter::new(budget);
    let result = (|| {
        meter.check()?;
        let plan = lexical::validate_plan(query, plan)?;
        if !matches!(plan.mode, SearchMode::Literal | SearchMode::Lexical)
            || catalog.operation_state()?.is_none()
        {
            return Err(WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "selected search requires normalized literal or lexical discovery",
            ));
        }
        catalog.guard_query()?;
        meter.check()?;
        let reader = catalog.cached_query_snapshot(QueryReadLimits {
            max_elapsed_ms: meter.remaining_ms(),
            ..Default::default()
        })?;
        let result = (|| {
            let mut hits = if ordinary_discovery {
                lexical::search_ordinary_lexical_catalog(&reader, query, &plan)?
            } else {
                lexical::search_catalog(&reader, query, &plan)?
            };
            meter.check()?;
            let paths = hits
                .hits
                .iter()
                .map(|hit| hit.locator.path.clone())
                .collect::<Vec<_>>();
            let remaining = VerificationBudget {
                max_elapsed_ms: meter.remaining_ms(),
                ..budget.clone()
            };
            let mut proof = selected_documents::authenticate(catalog, &reader, &paths, &remaining)?;
            let result = (|| {
                for hit in &mut hits.hits {
                    meter.check()?;
                    bind_hit(hit, &proof, catalog.vault_id())?;
                }
                hits.dependency_fingerprint = proof.fingerprint.clone();
                hits.verification = indexed_documents::verification(&reader)?;
                hits.warnings.push(SCOPE_WARNING.into());
                // Formatting belongs to the same deadline, before the final canonical recheck.
                serde_json::to_vec(&hits).map_err(serialization)?;
                reader.check_query_budget()?;
                #[cfg(test)]
                before_final()?;
                proof.recheck(catalog, &reader)?;
                reader.check_query_budget()?;
                meter.check()?;
                Ok(hits)
            })();
            #[cfg(test)]
            {
                stats.proof_work = Some(proof.meter().work());
            }
            result
        })();
        #[cfg(test)]
        {
            stats.catalog_rows = reader.usage().rows;
            stats.catalog_bytes = reader.usage().bytes;
        }
        result
    })();
    result
}

/// Validate a request without opening any catalog, proving evidence or producing hits.
pub fn preview(query: &str, plan: &QueryPlan) -> Result<serde_json::Value> {
    let plan = lexical::validate_plan(query, plan)?;
    if !matches!(
        plan.mode,
        SearchMode::Literal | SearchMode::Lexical | SearchMode::Semantic | SearchMode::Hybrid
    ) {
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "selected search planning requires literal, lexical, semantic or hybrid document mode",
        ));
    }
    Ok(
        serde_json::json!({"query":query,"plan":plan,"dry_run":true,"hits":null,"verification_performed":false,"cache_state_unknown":true,"database_opened":false,"admission_performed":false,"target_resolution_performed":false,"citations":[],"network_used":false}),
    )
}

#[cfg(test)]
pub(super) fn bind_hit_for_test(
    hit: &mut SearchHit,
    proof: &selected_documents::SelectedDocuments,
    vault: &RecordId,
) -> Result<()> {
    bind_hit(hit, proof, vault)
}

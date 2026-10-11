//! Present health separately from selected verification and current membership.
use crate::{
    catalog::health::{KnownHealth, PublishedHealth},
    domain::*,
    output::Envelope,
};
use serde_json::json;

pub(crate) fn attach_sync(
    envelope: &mut Envelope,
    health: &PublishedHealth,
    canonical_inspected: bool,
) -> Result<()> {
    envelope.data["completion"] = json!(if canonical_inspected {
        if health.has_issues {
            "completed_with_issues"
        } else {
            "completed"
        }
    } else {
        "published_diagnostics_only"
    });
    envelope.data["health"] = json!({
        "schema_version":"lwiki.health.v1",
        "observation":"published_generation",
        "index_generation":health.index_generation,
        "canonical_inspection_performed":canonical_inspected,
        "known_issues":health.has_issues,
        "discovery":{"complete_in_publication":true,"current_observation_complete":canonical_inspected},
        "structured_eligibility":{"excluded_document_count":health.excluded_document_count,"inspection_only_document_count":health.inspection_only_document_count,"complete":health.excluded_document_count==health.inspection_only_document_count,"corpus":"adopted_records_and_captured_revisions"},
        "membership_freshness":if canonical_inspected {"observed_by_completed_sync"} else {"unknown"},
        "diagnostic_count":health.diagnostic_count,
        "diagnostics":health.diagnostics,
        "diagnostics_truncated":health.diagnostics_truncated,
        "diagnostic_continuation":health.continuation,
        "omitted_detail_rows":health.omitted_detail_rows,
    });
    if health.has_issues {
        envelope.warnings.push(format!("Published generation {} has {} knowledge diagnostics; {} documents are excluded by invalid/unsupported eligibility. Healthy selected dependencies remain usable; inspect affected paths and run check for strict integrity.",health.index_generation,health.diagnostic_count,health.excluded_document_count));
    }
    if health.diagnostics_truncated {
        envelope.warnings.push("Diagnostics are paginated; continue with doctor --diagnostics --diagnostic-cursor using diagnostic_continuation.".into());
    }
    Ok(())
}

pub(crate) fn attach_known_read(envelope: &mut Envelope, health: &KnownHealth) -> Result<()> {
    envelope.data["health"] = json!({
        "schema_version":"lwiki.health.v1",
        "observation":"published_generation",
        "index_generation":health.index_generation,
        "known_issues":health.has_issues,
        "current_health":"unknown",
        "global_membership_freshness":"unknown",
        "selected_verification":envelope.meta.freshness,
        "structured_eligibility_complete":null,
    });
    if health.has_issues {
        envelope.warnings.push(format!("Generation {} has known knowledge diagnostics. This read's selected verification is separate; current global health and membership were not inspected. Use doctor --diagnostics for bounded diagnostics.",health.index_generation));
    }
    Ok(())
}

pub(crate) fn attach_preview(envelope: &mut Envelope, canonical_inspected: bool) {
    envelope.data["completion"] = json!("preview");
    envelope.data["health"] = json!({"schema_version":"lwiki.health.v1","canonical_inspection_performed":canonical_inspected,"known_issues":null,"discovery_complete":null,"structured_eligibility_complete":null,"membership_freshness":"unknown"});
}

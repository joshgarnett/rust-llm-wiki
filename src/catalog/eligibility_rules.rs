//! Shared dynamic rules. Callers prove a complete bounded semantic boundary;
//! these helpers neither discover membership nor validate a partial graph.
use super::{
    eligibility_facts::EligibilityBaseline,
    scan::{diagnostic, list},
    types::{CatalogDiagnostic, RecordRow},
};
use crate::domain::{
    CanonicalRecord, Eligibility, ErrorCode, RecordId, RecordKind, Result, WikiError,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

fn boundary_error(message: impl Into<String>) -> WikiError {
    WikiError::new(ErrorCode::IndexCorrupt, message)
}
fn set(row: &mut RecordRow, eligibility: Eligibility, reason: &str) {
    row.eligibility = eligibility;
    row.reasons.push(reason.into());
}
fn required<'a>(
    records: &'a BTreeMap<RecordId, RecordRow>,
    id: &str,
    kind: RecordKind,
) -> Result<&'a CanonicalRecord> {
    let id = RecordId::new(id)?;
    records
        .get(&id)
        .map(|row| &row.record)
        .filter(|record| record.id() == &id && record.kind() == kind)
        .ok_or_else(|| {
            boundary_error(format!(
                "missing or mismatched dynamic eligibility boundary: {id}"
            ))
        })
}
fn field<'a>(record: &'a CanonicalRecord, name: &str) -> Result<&'a str> {
    record
        .string(name)
        .ok_or_else(|| boundary_error(format!("dynamic eligibility lacks {name}")))
}

/// The caller authenticates generation output body/run/fingerprint binding.
/// Bound+None denotes a proven missing/wrong-kind source, never an unqueried one.
#[derive(Clone, Copy)]
pub(crate) enum GenerationContext<'a> {
    NotGeneration,
    Unbound,
    Bound {
        packet: &'a CanonicalRecord,
        source: Option<&'a CanonicalRecord>,
    },
}

pub(crate) fn restore_baseline(row: &mut RecordRow, baseline: &EligibilityBaseline) {
    row.eligibility = baseline.eligibility;
    row.reasons = baseline.reasons.clone();
    row.identity_eligibility = baseline.identity_eligibility;
    row.description_eligibility = baseline.description_eligibility;
    row.disputed = baseline.disputed;
}

pub(crate) fn lifecycle(
    row: &mut RecordRow,
    boundary: &BTreeMap<RecordId, RecordRow>,
    generation: GenerationContext<'_>,
) -> Result<Option<CatalogDiagnostic>> {
    if row.eligibility == Eligibility::Invalid {
        return Ok(None);
    }
    let record = &row.record;
    match record.kind() {
        RecordKind::Source if record.string("wiki_status") == Some("withdrawn") => {
            set(row, Eligibility::Withdrawn, "source_withdrawn")
        }
        RecordKind::Revision => {
            let source = required(
                boundary,
                field(record, "wiki_source_id")?,
                RecordKind::Source,
            )?;
            if source.string("wiki_status") == Some("withdrawn") {
                set(row, Eligibility::Withdrawn, "source_withdrawn");
            } else if source.string("wiki_current_revision") != Some(record.id().as_str()) {
                set(row, Eligibility::Historical, "older_revision");
            } else if record.string("wiki_extraction_status") != Some("complete") {
                set(row, Eligibility::Unsupported, "unsupported_extraction");
            }
        }
        RecordKind::ExtractionPacket => {
            let source = required(
                boundary,
                field(record, "wiki_source_id")?,
                RecordKind::Source,
            )?;
            if source.string("wiki_status") == Some("withdrawn") {
                set(row, Eligibility::Withdrawn, "source_withdrawn");
            } else if source.string("wiki_current_revision")
                != record.string("wiki_source_revision")
            {
                set(row, Eligibility::Historical, "older_revision");
            } else {
                set(row, Eligibility::Unsupported, "operational_packet");
            }
        }
        RecordKind::Extraction => {
            let revisions = list(record, "wiki_source_revision_ids");
            if revisions.is_empty() {
                set(row, Eligibility::Unsupported, "operational_extraction");
            } else {
                let mut withdrawn = false;
                let mut older = false;
                for revision_id in revisions {
                    let revision = required(boundary, &revision_id, RecordKind::Revision)?;
                    let source = required(
                        boundary,
                        field(revision, "wiki_source_id")?,
                        RecordKind::Source,
                    )?;
                    withdrawn |= source.string("wiki_status") == Some("withdrawn");
                    older |= source.string("wiki_current_revision") != Some(revision_id.as_str());
                }
                if withdrawn {
                    set(row, Eligibility::Withdrawn, "source_withdrawn");
                } else if older {
                    set(row, Eligibility::Historical, "older_revision");
                } else {
                    set(row, Eligibility::Unsupported, "operational_extraction");
                }
            }
        }
        RecordKind::RunEvent if record.string("wiki_event_type") == Some("generation_output") => {
            match generation {
                GenerationContext::NotGeneration => {
                    return Err(boundary_error(
                        "generation output requires explicit bound/unbound context",
                    ));
                }
                GenerationContext::Unbound => {
                    set(row, Eligibility::Unsupported, "unbound_generation_output")
                }
                GenerationContext::Bound { packet, source } => {
                    if packet.kind() != RecordKind::ExtractionPacket {
                        return Err(boundary_error(
                            "generation context is not an extraction packet",
                        ));
                    }
                    if let Some(source) = source {
                        if source.kind() != RecordKind::Source
                            || packet.string("wiki_source_id") != Some(source.id().as_str())
                        {
                            return Err(boundary_error(
                                "generation source boundary differs from packet",
                            ));
                        }
                        if source.string("wiki_status") == Some("withdrawn") {
                            set(row, Eligibility::Withdrawn, "source_withdrawn");
                        } else if source.string("wiki_current_revision")
                            != packet.string("wiki_source_revision")
                        {
                            set(row, Eligibility::Historical, "older_revision");
                        } else {
                            set(
                                row,
                                Eligibility::Unsupported,
                                "operational_generation_output",
                            );
                        }
                    } else {
                        set(
                            row,
                            Eligibility::Unsupported,
                            "generation_output_source_unresolved",
                        );
                        return Ok(Some(diagnostic(
                            &row.path,
                            Some(row.record.id()),
                            ErrorCode::RecordInvalid,
                            serde_json::json!({"reason":"generation_output_source_unresolved","packet":packet.id()}),
                        )));
                    }
                }
            }
        }
        RecordKind::Run | RecordKind::RunEvent | RecordKind::Change => {
            set(row, Eligibility::Unsupported, "operational_record")
        }
        RecordKind::Entity => {
            let identity = if record.string("wiki_status") == Some("superseded") {
                Eligibility::Historical
            } else {
                Eligibility::Current
            };
            row.identity_eligibility = Some(identity);
            if identity == Eligibility::Historical {
                set(row, Eligibility::Historical, "identity_superseded");
            } else if list(record, "wiki_depends_on_ids").is_empty() {
                set(row, Eligibility::Unsupported, "description_without_support");
            }
        }
        RecordKind::Assertion => match field(record, "wiki_status")? {
            "proposed" => set(row, Eligibility::Unsupported, "proposed"),
            "rejected" | "superseded" => set(
                row,
                Eligibility::Historical,
                "assertion_rejected_or_superseded",
            ),
            _ => {}
        },
        RecordKind::Page if record.string("wiki_status") == Some("deprecated") => {
            set(row, Eligibility::Historical, "page_deprecated")
        }
        RecordKind::Decision if record.string("wiki_status") == Some("superseded") => {
            set(row, Eligibility::Historical, "decision_superseded")
        }
        _ => {}
    }
    Ok(None)
}

pub(crate) fn evidence_lifecycle(row: &mut RecordRow, source: &CanonicalRecord) -> Result<()> {
    if row.record.kind() != RecordKind::Evidence {
        return Err(boundary_error("evidence lifecycle requires evidence"));
    }
    if row.eligibility == Eligibility::Invalid {
        return Ok(());
    }
    if source.kind() != RecordKind::Source
        || row.record.string("wiki_source_id") != Some(source.id().as_str())
    {
        return Err(boundary_error(
            "evidence source boundary differs from owner",
        ));
    }
    if row.record.string("wiki_status") == Some("retracted") {
        set(row, Eligibility::Historical, "evidence_retracted");
    } else if source.string("wiki_status") == Some("withdrawn") {
        set(row, Eligibility::Withdrawn, "source_withdrawn");
    } else if source.string("wiki_current_revision") != row.record.string("wiki_source_revision") {
        set(row, Eligibility::Historical, "older_revision");
    }
    Ok(())
}

/// expected_ids comes from a completely enumerated support membership group;
/// no caller may synthesize it merely from the rows it happened to load.
pub(crate) fn assertion_support(
    row: &mut RecordRow,
    expected_ids: &BTreeSet<RecordId>,
    associated: &[&RecordRow],
) -> Result<()> {
    if row.record.kind() != RecordKind::Assertion {
        return Err(boundary_error("support aggregation requires assertion"));
    }
    let mut actual = BTreeSet::new();
    for evidence in associated {
        if evidence.record.kind() != RecordKind::Evidence
            || evidence.record.string("wiki_assertion_id") != Some(row.record.id().as_str())
            || !actual.insert(evidence.record.id().clone())
        {
            return Err(boundary_error(
                "support group has duplicate or foreign evidence",
            ));
        }
    }
    if &actual != expected_ids {
        return Err(boundary_error("support group is incomplete"));
    }
    if row.eligibility == Eligibility::Invalid
        || row.record.string("wiki_status") != Some("accepted")
    {
        return Ok(());
    }
    row.disputed = associated.iter().any(|r| {
        r.eligibility == Eligibility::Current
            && r.record.string("wiki_stance") == Some("contradicts")
    });
    let support: Vec<_> = associated
        .iter()
        .filter(|r| r.record.string("wiki_stance") == Some("supports"))
        .collect();
    if support
        .iter()
        .any(|r| r.eligibility == Eligibility::Current)
    {
        set(row, Eligibility::Current, "current_support");
    } else if support.iter().any(|r| {
        r.eligibility == Eligibility::Historical
            && r.reasons.iter().any(|reason| reason == "older_revision")
    }) {
        set(row, Eligibility::Stale, "only_historical_support");
    } else {
        set(row, Eligibility::Unsupported, "no_current_support");
        for cause in [
            "source_withdrawn",
            "evidence_retracted",
            "evidence_integrity",
        ] {
            if support
                .iter()
                .any(|r| r.reasons.iter().any(|reason| reason == cause))
            {
                row.reasons.push(cause.into());
            }
        }
        if support.is_empty() {
            row.reasons.push("support_absent".into());
        }
    }
    if row.disputed {
        row.reasons.push("disputed".into());
    }
    Ok(())
}

/// Each declared ID must have an entry. None is proven missing; omitted keys
/// are unknown boundary facts and cause refusal rather than invalidation.
pub(crate) fn declared_dependencies(
    row: &mut RecordRow,
    targets: &BTreeMap<RecordId, Option<Eligibility>>,
) -> Result<bool> {
    let declared: BTreeSet<_> = list(&row.record, "wiki_depends_on_ids")
        .into_iter()
        .map(RecordId::new)
        .collect::<Result<_>>()?;
    if declared.iter().ne(targets.keys()) {
        return Err(boundary_error(
            "declared dependency boundary is incomplete or extraneous",
        ));
    }
    if matches!(
        row.eligibility,
        Eligibility::Invalid | Eligibility::Historical
    ) {
        return Ok(false);
    }
    let invalid = targets
        .values()
        .any(|state| state.is_none_or(|eligibility| eligibility == Eligibility::Invalid));
    let stale = targets
        .values()
        .any(|state| state.is_some_and(|eligibility| eligibility != Eligibility::Current));
    let next = if invalid {
        Some((Eligibility::Invalid, "invalid_dependency"))
    } else if stale {
        Some((Eligibility::Stale, "ineligible_dependency"))
    } else {
        None
    };
    if let Some((eligibility, reason)) = next {
        let changed = row.eligibility != eligibility;
        set(row, eligibility, reason);
        Ok(changed)
    } else {
        Ok(false)
    }
}

pub(crate) fn mark_opposition(row: &mut RecordRow) {
    row.disputed = true;
    row.reasons.push("opposing_accepted_assertion".into());
    row.reasons.push("disputed".into());
}
pub(crate) fn finalize(row: &mut RecordRow) {
    row.reasons.sort();
    row.reasons.dedup();
    if row.record.kind() == RecordKind::Entity {
        row.description_eligibility = Some(row.eligibility);
        row.identity_eligibility = Some(if entity_identity_invalid(row) {
            Eligibility::Invalid
        } else if row.record.string("wiki_status") == Some("superseded") {
            Eligibility::Historical
        } else {
            Eligibility::Current
        });
    }
}

/// The exact authored proposition apart from polarity. This is deliberately
/// narrower than a semantic contradiction: dates and modality must be equal.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OppositionKey {
    subject: String,
    predicate: String,
    object_id: Option<String>,
    literal_type: Option<String>,
    literal_value: Option<String>,
    property: Option<String>,
    unit: Option<String>,
    modality: String,
    valid_from: Option<String>,
    valid_until: Option<String>,
}

pub(crate) fn opposition_key(record: &CanonicalRecord) -> Option<(OppositionKey, bool)> {
    if record.kind() != RecordKind::Assertion {
        return None;
    }
    Some((
        OppositionKey {
            subject: record.string("wiki_subject_id")?.into(),
            predicate: record.string("wiki_predicate")?.into(),
            object_id: record.string("wiki_object_id").map(str::to_owned),
            literal_type: record.string("wiki_literal_type").map(str::to_owned),
            literal_value: record.string("wiki_literal_value").map(str::to_owned),
            property: record.string("wiki_property").map(str::to_owned),
            unit: record.string("wiki_unit").map(str::to_owned),
            modality: record.string("wiki_modality").unwrap_or("asserted").into(),
            valid_from: record.string("wiki_valid_from").map(str::to_owned),
            valid_until: record.string("wiki_valid_until").map(str::to_owned),
        },
        record
            .field("wiki_negated")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    ))
}

pub(crate) fn eligible_opposition(row: &RecordRow) -> bool {
    row.record.kind() == RecordKind::Assertion
        && row.record.string("wiki_status") == Some("accepted")
        && row.eligibility == Eligibility::Current
}

pub(crate) fn entity_identity_invalid(row: &RecordRow) -> bool {
    row.eligibility == Eligibility::Invalid
        && row.reasons.iter().any(|reason| {
            !matches!(
                reason.as_str(),
                "invalid_dependency_reference"
                    | "dependency_cycle"
                    | "invalid_dependency"
                    | "ineligible_dependency"
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Blake3Hash, VaultRelativePath};
    use serde_json::json;
    fn id(value: &str) -> RecordId {
        RecordId::new(value).unwrap()
    }
    fn row(kind: &str, name: &str, extra: serde_json::Value) -> RecordRow {
        let mut fields = BTreeMap::from([
            ("wiki_schema".into(), json!("1")),
            ("wiki_id".into(), json!(name)),
            ("wiki_kind".into(), json!(kind)),
            ("title".into(), json!(name)),
        ]);
        fields.extend(
            extra
                .as_object()
                .unwrap()
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
        let record = CanonicalRecord::new(fields).unwrap();
        RecordRow {
            authored_status: record.string("wiki_status").map(str::to_owned),
            record,
            path: VaultRelativePath::new(format!("{name}.md")).unwrap(),
            hash: Blake3Hash::digest(name),
            eligibility: Eligibility::Current,
            reasons: vec![],
            identity_eligibility: None,
            description_eligibility: None,
            disputed: false,
            dependencies: vec![],
        }
    }
    fn baseline() -> EligibilityBaseline {
        EligibilityBaseline {
            eligibility: Eligibility::Current,
            reasons: vec![],
            identity_eligibility: None,
            description_eligibility: None,
            disputed: false,
        }
    }
    fn source(head: &str) -> RecordRow {
        row(
            "source",
            "source_one",
            json!({"wiki_status":"active","wiki_origin_kind":"local-file","wiki_origin":"fixture","wiki_current_revision":head,"wiki_revisions":["revision_one","revision_two"]}),
        )
    }
    fn evidence(name: &str, assertion: &str, stance: &str) -> RecordRow {
        row(
            "evidence",
            name,
            json!({"wiki_status":"active","wiki_assertion_id":assertion,"wiki_source_id":"source_one","wiki_source_revision":"revision_one","wiki_stance":stance,"wiki_locator_kind":"utf8-bytes","wiki_span_start":0,"wiki_span_end":1,"wiki_quote_hash":Blake3Hash::digest(b"x")}),
        )
    }
    fn assertion(name: &str) -> RecordRow {
        row(
            "assertion",
            name,
            json!({"wiki_status":"accepted","wiki_subject_id":"entity_subject","wiki_object_id":"entity_object","wiki_predicate":"uses"}),
        )
    }

    #[test]
    fn incomplete_lifecycle_boundary_refuses_and_historical_reset_can_promote() {
        let hash = Blake3Hash::digest(b"fixture");
        let mut revision = row(
            "revision",
            "revision_one",
            json!({"wiki_source_id":"source_one","wiki_captured_at":"2026-09-28T00:00:00Z","wiki_original_path":"original.bin","wiki_original_hash":hash,"wiki_extractor":"fixture","wiki_extractor_fingerprint":hash,"wiki_extraction_status":"complete","wiki_content_path":"content.md","wiki_content_hash":hash}),
        );
        assert!(
            lifecycle(
                &mut revision,
                &BTreeMap::new(),
                GenerationContext::NotGeneration
            )
            .is_err()
        );
        let boundary = BTreeMap::from([(id("source_one"), source("revision_two"))]);
        lifecycle(&mut revision, &boundary, GenerationContext::NotGeneration).unwrap();
        assert_eq!(revision.eligibility, Eligibility::Historical);
        assert_eq!(revision.reasons, ["older_revision"]);
        restore_baseline(&mut revision, &baseline());
        lifecycle(
            &mut revision,
            &BTreeMap::from([(id("source_one"), source("revision_one"))]),
            GenerationContext::NotGeneration,
        )
        .unwrap();
        finalize(&mut revision);
        assert_eq!(revision.eligibility, Eligibility::Current);
        assert!(revision.reasons.is_empty());
        let mut support = evidence("evidence_one", "assertion_one", "supports");
        evidence_lifecycle(&mut support, &source("revision_two").record).unwrap();
        assert_eq!(support.eligibility, Eligibility::Historical);
        restore_baseline(&mut support, &baseline());
        evidence_lifecycle(&mut support, &source("revision_one").record).unwrap();
        assert_eq!(support.eligibility, Eligibility::Current);
        assert!(support.reasons.is_empty());
    }

    #[test]
    fn complete_support_preserves_intact_success_and_rejects_omitted_or_foreign_members() {
        let good = evidence("evidence_good", "assertion_one", "supports");
        let mut invalid = evidence("evidence_bad", "assertion_one", "supports");
        invalid.eligibility = Eligibility::Invalid;
        invalid.reasons.push("evidence_integrity".into());
        let contradiction = evidence("evidence_contra", "assertion_one", "contradicts");
        let expected = BTreeSet::from([
            good.record.id().clone(),
            invalid.record.id().clone(),
            contradiction.record.id().clone(),
        ]);
        let mut claim = assertion("assertion_one");
        assert!(assertion_support(&mut claim, &expected, &[&invalid, &contradiction]).is_err());
        assert!(assertion_support(&mut claim, &expected, &[&good, &good, &contradiction]).is_err());
        let foreign = evidence("evidence_bad", "assertion_other", "supports");
        assert!(
            assertion_support(&mut claim, &expected, &[&good, &foreign, &contradiction]).is_err()
        );
        assertion_support(&mut claim, &expected, &[&good, &invalid, &contradiction]).unwrap();
        finalize(&mut claim);
        assert_eq!(claim.eligibility, Eligibility::Current);
        assert!(claim.disputed);
        assert_eq!(claim.reasons, ["current_support", "disputed"]);
        let mut old = good.clone();
        old.eligibility = Eligibility::Historical;
        old.reasons.push("older_revision".into());
        restore_baseline(&mut claim, &baseline());
        assertion_support(
            &mut claim,
            &BTreeSet::from([old.record.id().clone()]),
            &[&old],
        )
        .unwrap();
        assert_eq!(claim.eligibility, Eligibility::Stale);
        assert!(!claim.disputed);
        assert_eq!(claim.reasons, ["only_historical_support"]);
    }

    #[test]
    fn unknown_dependency_differs_from_proven_missing_and_entity_identity_survives() {
        let mut entity = row(
            "entity",
            "entity_one",
            json!({"wiki_status":"active","wiki_entity_type":"component","wiki_depends_on_ids":["assertion_one"]}),
        );
        let unchanged = entity.clone();
        assert!(declared_dependencies(&mut entity, &BTreeMap::new()).is_err());
        assert_eq!(entity, unchanged);
        assert!(
            declared_dependencies(&mut entity, &BTreeMap::from([(id("assertion_one"), None)]))
                .unwrap()
        );
        finalize(&mut entity);
        assert_eq!(entity.description_eligibility, Some(Eligibility::Invalid));
        assert_eq!(entity.identity_eligibility, Some(Eligibility::Current));
        restore_baseline(&mut entity, &baseline());
        assert!(
            !declared_dependencies(
                &mut entity,
                &BTreeMap::from([(id("assertion_one"), Some(Eligibility::Current))])
            )
            .unwrap()
        );
        finalize(&mut entity);
        assert_eq!(entity.description_eligibility, Some(Eligibility::Current));
        assert!(entity.reasons.is_empty());
        let mut static_invalid = baseline();
        static_invalid.eligibility = Eligibility::Invalid;
        static_invalid.reasons = vec!["decision_outcome_disagreement".into()];
        restore_baseline(&mut entity, &static_invalid);
        declared_dependencies(
            &mut entity,
            &BTreeMap::from([(id("assertion_one"), Some(Eligibility::Current))]),
        )
        .unwrap();
        finalize(&mut entity);
        assert_eq!(entity.identity_eligibility, Some(Eligibility::Invalid));
    }

    #[test]
    fn generation_output_requires_explicit_binding_and_preserves_unresolved_diagnostic() {
        let mut event = row(
            "run_event",
            "event_one",
            json!({"wiki_run_id":"run_one","wiki_sequence":1,"wiki_event_type":"generation_output","wiki_occurred_at":"2026-09-28T00:00:00Z"}),
        );
        let packet_id = RecordId::packet(&Blake3Hash::digest(b"packet"));
        let packet = row(
            "extraction_packet",
            packet_id.as_str(),
            json!({"wiki_source_id":"source_one","wiki_source_revision":"revision_one","wiki_packet_fingerprint":Blake3Hash::digest(b"packet"),"wiki_output_schema":"fixture","wiki_created_at":"2026-09-28T00:00:00Z"}),
        );
        assert!(
            lifecycle(
                &mut event,
                &BTreeMap::new(),
                GenerationContext::NotGeneration
            )
            .is_err()
        );
        let diagnostic = lifecycle(
            &mut event,
            &BTreeMap::new(),
            GenerationContext::Bound {
                packet: &packet.record,
                source: None,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            diagnostic.details,
            json!({"reason":"generation_output_source_unresolved","packet":packet_id})
        );
        assert_eq!(event.reasons, ["generation_output_source_unresolved"]);
        restore_baseline(&mut event, &baseline());
        lifecycle(
            &mut event,
            &BTreeMap::new(),
            GenerationContext::Bound {
                packet: &packet.record,
                source: Some(&source("revision_two").record),
            },
        )
        .unwrap();
        assert_eq!(event.eligibility, Eligibility::Historical);
        assert_eq!(event.reasons, ["older_revision"]);
        restore_baseline(&mut event, &baseline());
        lifecycle(&mut event, &BTreeMap::new(), GenerationContext::Unbound).unwrap();
        assert_eq!(event.reasons, ["unbound_generation_output"]);
    }

    #[test]
    fn opposition_key_roundtrip_and_reset_keep_polarity_and_dispute_separate() {
        let mut claim = assertion("assertion_one");
        let (key, negated) = opposition_key(&claim.record).unwrap();
        assert!(!negated);
        let json = serde_json::to_string(&key).unwrap();
        assert_eq!(serde_json::from_str::<OppositionKey>(&json).unwrap(), key);
        let mut fields = claim.record.fields().clone();
        fields.insert("wiki_negated".into(), json!(true));
        claim.record = CanonicalRecord::new(fields).unwrap();
        assert_eq!(opposition_key(&claim.record).unwrap(), (key, true));
        mark_opposition(&mut claim);
        finalize(&mut claim);
        assert!(claim.disputed);
        restore_baseline(&mut claim, &baseline());
        finalize(&mut claim);
        assert!(!claim.disputed);
        assert!(claim.reasons.is_empty());
        let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
        value["unknown"] = json!(true);
        assert!(serde_json::from_value::<OppositionKey>(value).is_err());
    }
}

/// Navigation diagnostics do not change assertion support or structural state.
/// A resolved target comes from the caller's complete registry lookup.
pub(crate) fn evidence_navigation_reason(
    assertion: &RecordId,
    resolution: &crate::records::LinkResolution,
    target: Option<&CanonicalRecord>,
) -> Option<&'static str> {
    compact_evidence_navigation_reason(
        assertion,
        &super::navigation_resolution::NavigationResolution::from(resolution),
        target,
    )
}

pub(crate) fn compact_evidence_navigation_reason(
    assertion: &RecordId,
    resolution: &super::navigation_resolution::NavigationResolution,
    target: Option<&CanonicalRecord>,
) -> Option<&'static str> {
    match resolution {
        super::navigation_resolution::NavigationResolution::Resolved { .. } => match target {
            Some(evidence) if evidence.kind() == RecordKind::Evidence => {
                if evidence.string("wiki_assertion_id") == Some(assertion.as_str()) {
                    None
                } else {
                    Some("evidence_link_wrong_assertion")
                }
            }
            Some(_) => Some("evidence_link_wrong_kind"),
            None => Some("evidence_link_missing"),
        },
        super::navigation_resolution::NavigationResolution::Ambiguous => {
            Some("evidence_link_ambiguous")
        }
        _ => Some("evidence_link_missing"),
    }
}

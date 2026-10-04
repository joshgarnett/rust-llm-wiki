//! Exact shared local structural checks and stage provenance. A selected caller
//! supplies complete boundaries; this module never scans a vault or runs compute.
use super::{
    eligibility::references,
    eligibility_facts::{EligibilityBaseline, EligibilityEdge, EligibilityRole},
    eligibility_rules::entity_identity_invalid,
    scan::{diagnostic, list},
    types::{CatalogDiagnostic, RecordRow},
};
use crate::{
    domain::{Eligibility, ErrorCode, RecordId, RecordKind, Result, WikiError},
    records::{LinkResolution, links::IndexedRegistry},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StructuralStage {
    Reference,
    Invariant,
    Decision,
    RevisionIntegrity,
    Propagation,
    EvidenceIntegrity,
}
impl StructuralStage {
    /// Execution phase order is explicit, independent of enum declaration/serde.
    pub(crate) const fn rank(self) -> u8 {
        match self {
            Self::Reference => 0,
            Self::Invariant => 1,
            Self::Decision => 2,
            Self::RevisionIntegrity => 3,
            Self::Propagation => 4,
            Self::EvidenceIntegrity => 5,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StructuralEffect {
    pub stage: StructuralStage,
    /// Decision ID or overlap target for replaceable nonlocal outcome effects.
    pub producer: Option<RecordId>,
    pub eligibility: Eligibility,
    pub reason: String,
    /// None means a state/reason without a diagnostic; Some(Null) is a diagnostic.
    #[serde(with = "effect_details")]
    pub details: Option<Value>,
}
// Serde's ordinary Option<Value> maps both None and Some(Null) to null.
// The wrapper preserves an emitted null-details diagnostic across retention.
mod effect_details {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use serde_json::Value;
    #[derive(Serialize)]
    struct DetailRef<'a> {
        value: &'a Value,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Detail {
        value: Value,
    }
    pub(super) fn serialize<S: Serializer>(
        value: &Option<Value>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value
            .as_ref()
            .map(|value| DetailRef { value })
            .serialize(serializer)
    }
    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Value>, D::Error> {
        Ok(Option::<Detail>::deserialize(deserializer)?.map(|detail| detail.value))
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StructuralFact {
    pub effects: Vec<StructuralEffect>,
}
impl StructuralFact {
    /// Replay only the requested inclusive prefix. Later-stage effects remain
    /// retained but cannot leak final eligibility into an earlier phase.
    pub(crate) fn baseline_through(&self, through: StructuralStage) -> Result<EligibilityBaseline> {
        self.validate_order()?;
        Ok(Self {
            effects: self
                .effects
                .iter()
                .filter(|effect| effect.stage.rank() <= through.rank())
                .cloned()
                .collect(),
        }
        .baseline())
    }
    /// Replace an entire stage, preserving emission order within that stage and
    /// the exact order of all other effects. Later effects are retained, not
    /// revalidated: callers must reevaluate dependent stages before publication.
    pub(crate) fn replace_stage_effects(
        &mut self,
        stage: StructuralStage,
        replacements: Vec<StructuralEffect>,
    ) -> Result<()> {
        self.validate_order()?;
        if replacements.iter().any(|effect| effect.stage != stage) {
            return Err(incomplete(
                "replacement effects belong to another structural stage",
            ));
        }
        let before = self
            .effects
            .partition_point(|effect| effect.stage.rank() < stage.rank());
        let after = self
            .effects
            .partition_point(|effect| effect.stage.rank() <= stage.rank());
        self.effects.splice(before..after, replacements);
        Ok(())
    }
    fn validate_order(&self) -> Result<()> {
        if self
            .effects
            .windows(2)
            .any(|pair| pair[0].stage.rank() > pair[1].stage.rank())
        {
            return Err(incomplete(
                "structural effects are not in execution stage order",
            ));
        }
        Ok(())
    }
    pub(crate) fn baseline(&self) -> EligibilityBaseline {
        let mut eligibility = Eligibility::Current;
        let mut reasons = Vec::new();
        for effect in &self.effects {
            eligibility = effect.eligibility;
            reasons.push(effect.reason.clone());
        }
        reasons.sort();
        reasons.dedup();
        EligibilityBaseline {
            eligibility,
            reasons,
            identity_eligibility: None,
            description_eligibility: None,
            disputed: false,
        }
    }
    pub(crate) fn diagnostics(&self, row: &RecordRow) -> Vec<CatalogDiagnostic> {
        self.effects
            .iter()
            .filter_map(|effect| {
                effect.details.as_ref().map(|details| {
                    diagnostic(
                        &row.path,
                        Some(row.record.id()),
                        ErrorCode::RecordInvalid,
                        serde_json::json!({"reason":effect.reason,"details":details}),
                    )
                })
            })
            .collect()
    }
}
/// Recording is disabled for legacy projection so the shared rules do not add
/// a second retained graph to its memory use.
pub(crate) struct StructuralRecorder {
    enabled: bool,
    stage: StructuralStage,
    producer: Option<RecordId>,
    effects: BTreeMap<RecordId, StructuralFact>,
}
impl StructuralRecorder {
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            stage: StructuralStage::Reference,
            producer: None,
            effects: BTreeMap::new(),
        }
    }
    pub(crate) fn phase(&mut self, stage: StructuralStage) {
        self.stage = stage;
        self.producer = None;
    }
    pub(crate) fn producer(&mut self, id: Option<RecordId>) {
        self.producer = id;
    }
    pub(crate) fn finish(self) -> BTreeMap<RecordId, StructuralFact> {
        self.effects
    }
    fn record(
        &mut self,
        row: &RecordRow,
        eligibility: Eligibility,
        reason: &str,
        details: Option<Value>,
    ) {
        if self.enabled {
            self.effects
                .entry(row.record.id().clone())
                .or_default()
                .effects
                .push(StructuralEffect {
                    stage: self.stage,
                    producer: self.producer.clone(),
                    eligibility,
                    reason: reason.into(),
                    details,
                });
        }
    }
}
pub(crate) fn invalidate(
    row: &mut RecordRow,
    reason: impl Into<String>,
    diagnostics: &mut Vec<CatalogDiagnostic>,
    details: Value,
    recorder: &mut StructuralRecorder,
) {
    let reason = reason.into();
    recorder.record(row, Eligibility::Invalid, &reason, Some(details.clone()));
    row.eligibility = Eligibility::Invalid;
    if !row.reasons.contains(&reason) {
        row.reasons.push(reason.clone());
    }
    diagnostics.push(diagnostic(
        &row.path,
        Some(row.record.id()),
        ErrorCode::RecordInvalid,
        serde_json::json!({"reason":reason,"details":details}),
    ));
}
pub(crate) fn state(
    row: &mut RecordRow,
    eligibility: Eligibility,
    reason: &str,
    recorder: &mut StructuralRecorder,
) {
    recorder.record(row, eligibility, reason, None);
    row.eligibility = eligibility;
    row.reasons.push(reason.into());
}

/// Missing and unqueried targets are distinct. The full rebuild owns a complete
/// map; selected projection must name each proven-missing boundary explicitly.
pub(crate) struct ReferenceBoundary<'a> {
    rows: &'a BTreeMap<RecordId, RecordRow>,
    missing: Option<&'a BTreeSet<RecordId>>,
}
impl<'a> ReferenceBoundary<'a> {
    pub(crate) fn complete(rows: &'a BTreeMap<RecordId, RecordRow>) -> Self {
        Self {
            rows,
            missing: None,
        }
    }
    pub(crate) fn selected(
        rows: &'a BTreeMap<RecordId, RecordRow>,
        missing: &'a BTreeSet<RecordId>,
    ) -> Result<Self> {
        if missing.iter().any(|id| rows.contains_key(id)) {
            return Err(incomplete("contradictory structural boundary"));
        }
        Ok(Self {
            rows,
            missing: Some(missing),
        })
    }
    fn admit(&self, id: &RecordId) -> Result<()> {
        if self.rows.contains_key(id) || self.missing.is_none_or(|missing| missing.contains(id)) {
            Ok(())
        } else {
            Err(incomplete(format!(
                "structural target was not queried: {id}"
            )))
        }
    }
    fn get(&self, id: &RecordId) -> Option<&RecordRow> {
        self.rows.get(id)
    }
}
fn incomplete(message: impl Into<String>) -> WikiError {
    WikiError::new(ErrorCode::IndexCorrupt, message)
}
#[derive(Default)]
pub(crate) struct ReferenceEdges {
    pub targets: BTreeSet<RecordId>,
    pub declared: BTreeSet<RecordId>,
    pub supersession: BTreeSet<RecordId>,
    pub semantic: BTreeSet<EligibilityEdge>,
}
fn list_references(kind: RecordKind) -> Vec<(&'static str, Option<RecordKind>)> {
    match kind {
        RecordKind::Source => vec![("wiki_revisions", Some(RecordKind::Revision))],
        RecordKind::Extraction => vec![
            ("wiki_source_ids", Some(RecordKind::Source)),
            ("wiki_source_revision_ids", Some(RecordKind::Revision)),
        ],
        RecordKind::Decision => vec![("wiki_input_ids", None), ("wiki_output_ids", None)],
        _ => vec![],
    }
}
pub(crate) fn reference_ids(row: &RecordRow) -> Result<BTreeSet<RecordId>> {
    let mut ids = BTreeSet::new();
    for (field, _, _) in references(&row.record) {
        if let Some(id) = row.record.string(field) {
            ids.insert(RecordId::new(id)?);
        }
    }
    for (field, _) in list_references(row.record.kind())
        .into_iter()
        .chain(std::iter::once(("wiki_depends_on_ids", None)))
    {
        for id in list(&row.record, field) {
            ids.insert(RecordId::new(id)?);
        }
    }
    Ok(ids)
}
pub(crate) fn evaluate_references(
    row: &mut RecordRow,
    registry: &IndexedRegistry,
    snapshot: &ReferenceBoundary<'_>,
    diagnostics: &mut Vec<CatalogDiagnostic>,
    recorder: &mut StructuralRecorder,
) -> Result<ReferenceEdges> {
    for id in reference_ids(row)? {
        snapshot.admit(&id)?;
    }
    let record = row.record.clone();
    let id = record.id();
    let mut output = ReferenceEdges::default();
    for (field, kind, companion) in references(&record) {
        if let Some(value) = record.string(field) {
            let target = RecordId::new(value)?;
            output.targets.insert(target.clone());
            output.semantic.insert(EligibilityEdge {
                owner_id: id.clone(),
                target_id: target.clone(),
                role: EligibilityRole::TypedReference {
                    field: field.into(),
                },
            });
            if !matches!(
                registry.resolve_typed(&target, kind, companion.and_then(|k| record.string(k))),
                LinkResolution::Resolved { .. }
            ) {
                invalidate(
                    row,
                    format!("invalid_reference:{field}"),
                    diagnostics,
                    serde_json::json!({"target":target,"expected_kind":kind}),
                    recorder,
                );
            }
            if matches!(field, "wiki_supersedes_id" | "wiki_superseded_by_id") {
                output.supersession.insert(target);
            }
        }
    }
    for value in list(&record, "wiki_depends_on_ids") {
        let target = RecordId::new(value)?;
        output.targets.insert(target.clone());
        output.semantic.insert(EligibilityEdge {
            owner_id: id.clone(),
            target_id: target.clone(),
            role: EligibilityRole::DeclaredSupport,
        });
        output.declared.insert(target.clone());
        if !matches!(
            registry.resolve_typed(&target, RecordKind::Assertion, None),
            LinkResolution::Resolved { .. }
        ) {
            invalidate(
                row,
                "invalid_dependency_reference",
                diagnostics,
                serde_json::json!({"target":target}),
                recorder,
            );
        }
    }
    for (field, kind) in list_references(record.kind()) {
        let values = list(&record, field);
        let unique: BTreeSet<_> = values.iter().collect();
        if unique.len() != values.len() {
            invalidate(
                row,
                format!("duplicate_reference:{field}"),
                diagnostics,
                serde_json::Value::Null,
                recorder,
            );
        }
        for value in values {
            let target = RecordId::new(value)?;
            output.targets.insert(target.clone());
            {
                let role = match field {
                    "wiki_revisions" => EligibilityRole::SourceInventory,
                    "wiki_source_ids" => EligibilityRole::ExtractionSource,
                    "wiki_source_revision_ids" => EligibilityRole::ExtractionRevision,
                    "wiki_input_ids" => EligibilityRole::DecisionInput,
                    "wiki_output_ids" => EligibilityRole::DecisionOutput,
                    _ => unreachable!("fixed list reference vocabulary"),
                };
                output.semantic.insert(EligibilityEdge {
                    owner_id: id.clone(),
                    target_id: target.clone(),
                    role,
                });
            }
            if snapshot
                .get(&target)
                .is_none_or(|r| kind.is_some_and(|kind| r.record.kind() != kind))
            {
                invalidate(
                    row,
                    format!("invalid_reference:{field}"),
                    diagnostics,
                    serde_json::json!({"target":target}),
                    recorder,
                );
            }
            if record.kind() == RecordKind::Source
                && snapshot
                    .get(&target)
                    .is_some_and(|r| r.record.string("wiki_source_id") != Some(id.as_str()))
            {
                invalidate(
                    row,
                    "revision_ownership_mismatch",
                    diagnostics,
                    serde_json::json!({"target":target}),
                    recorder,
                );
            }
        }
    }
    if record.kind() == RecordKind::Source
        && !list(&record, "wiki_revisions")
            .iter()
            .any(|r| Some(r.as_str()) == record.string("wiki_current_revision"))
    {
        invalidate(
            row,
            "head_not_retained",
            diagnostics,
            serde_json::Value::Null,
            recorder,
        );
    }
    if record.kind() == RecordKind::Extraction {
        let source_set: BTreeSet<_> = list(&record, "wiki_source_ids").into_iter().collect();
        let owner_set: BTreeSet<_> = list(&record, "wiki_source_revision_ids")
            .iter()
            .filter_map(|id| RecordId::new(id).ok())
            .filter_map(|id| snapshot.get(&id))
            .filter_map(|r| r.record.string("wiki_source_id"))
            .map(str::to_owned)
            .collect();
        if source_set != owner_set {
            invalidate(
                row,
                "extraction_ownership_set_mismatch",
                diagnostics,
                serde_json::Value::Null,
                recorder,
            );
        }
    }
    if record.kind() == RecordKind::ExtractionPacket
        && snapshot
            .get(&RecordId::new(
                record.string("wiki_source_revision").expect("revision"),
            )?)
            .is_some_and(|r| r.record.string("wiki_source_id") != record.string("wiki_source_id"))
    {
        invalidate(
            row,
            "packet_revision_ownership_mismatch",
            diagnostics,
            serde_json::Value::Null,
            recorder,
        );
    }
    if record.kind() == RecordKind::Evidence
        && let Some(predecessor) = record
            .string("wiki_supersedes_id")
            .and_then(|id| RecordId::new(id).ok())
            .and_then(|id| snapshot.get(&id))
    {
        let same_chain = ["wiki_assertion_id", "wiki_source_id"]
            .iter()
            .all(|field| record.field(field) == predecessor.record.field(field));
        let same_revision = record.string("wiki_source_revision")
            == predecessor.record.string("wiki_source_revision");
        let same_span = [
            "wiki_locator_kind",
            "wiki_span_start",
            "wiki_span_end",
            "wiki_quote_hash",
        ]
        .iter()
        .all(|field| record.field(field) == predecessor.record.field(field));
        if !same_chain || same_revision && !same_span {
            invalidate(
                row,
                "evidence_successor_chain_disagreement",
                diagnostics,
                serde_json::json!({"predecessor":predecessor.record.id()}),
                recorder,
            );
        }
    }
    Ok(output)
}
/// Structural propagation uses entity identity, not its supported description.
pub(crate) fn structurally_invalid(row: &RecordRow) -> bool {
    row.eligibility == Eligibility::Invalid
        && (row.record.kind() != RecordKind::Entity || entity_identity_invalid(row))
}
pub(crate) fn propagate_invalid(
    records: &mut BTreeMap<RecordId, RecordRow>,
    edges: &BTreeMap<RecordId, BTreeSet<RecordId>>,
    diagnostics: &mut Vec<CatalogDiagnostic>,
    outside: &BTreeMap<RecordId, bool>,
    recorder: &mut StructuralRecorder,
) -> Result<()> {
    if outside.keys().any(|id| records.contains_key(id)) {
        return Err(incomplete("contradictory structural propagation boundary"));
    }
    for target in edges.values().flatten() {
        if !records.contains_key(target) && !outside.contains_key(target) {
            return Err(incomplete(format!(
                "structural propagation boundary omitted: {target}"
            )));
        }
    }
    loop {
        let mut invalid: BTreeSet<_> = records
            .iter()
            .filter(|(_, row)| structurally_invalid(row))
            .map(|(id, _)| id.clone())
            .collect();
        invalid.extend(
            outside
                .iter()
                .filter(|(_, invalid)| **invalid)
                .map(|(id, _)| id.clone()),
        );
        let mut changed = false;
        for (id, row) in records.iter_mut() {
            if row.eligibility != Eligibility::Invalid
                && edges
                    .get(id)
                    .is_some_and(|targets| targets.iter().any(|target| invalid.contains(target)))
            {
                // Declared description support invalidity never erases an otherwise
                // valid entity identity. The description pass handles that channel.
                if row.record.kind() == RecordKind::Entity
                    && references(&row.record).iter().all(|(field, _, _)| {
                        row.record.string(field).is_none_or(|target| {
                            RecordId::new(target).is_ok_and(|target| !invalid.contains(&target))
                        })
                    })
                {
                    continue;
                }
                invalidate(
                    row,
                    "invalid_referenced_record",
                    diagnostics,
                    serde_json::Value::Null,
                    recorder,
                );
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "structural_rules_tests.rs"]
mod tests;

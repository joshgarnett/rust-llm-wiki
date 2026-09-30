//! Canonical evidence selection. Public graph payloads are discovery selectors only.
use super::{context_types::*, types::*};
use crate::{
    catalog::{ReaderSnapshot, RecordRow},
    domain::*,
    graph::{GraphAssertion, GraphObject, GraphPathStep, GraphQualifiers},
    sources::EvidenceStance,
};
use std::collections::BTreeSet;

pub(super) fn reference(reader: &ReaderSnapshot, row: &RecordRow) -> RecordRef {
    RecordRef {
        vault_id: reader.projection().vault_id.clone(),
        record_id: row.record.id().clone(),
        expected_kind: row.record.kind(),
    }
}
pub(super) fn proposition(
    reader: &ReaderSnapshot,
    row: &RecordRow,
) -> Result<(RecordRef, String, GraphObject, GraphQualifiers)> {
    let id = RecordId::new(
        row.record
            .string("wiki_subject_id")
            .ok_or_else(|| WikiError::invalid("assertion subject missing"))?,
    )?;
    let subject = reader
        .projection()
        .records
        .get(&id)
        .filter(|r| r.record.kind() == RecordKind::Entity)
        .ok_or_else(|| WikiError::invalid("assertion subject unresolved"))?;
    let object = if let Some(value) = row.record.string("wiki_object_id") {
        let id = RecordId::new(value)?;
        let target = reader
            .projection()
            .records
            .get(&id)
            .filter(|r| r.record.kind() == RecordKind::Entity)
            .ok_or_else(|| WikiError::invalid("assertion object unresolved"))?;
        GraphObject::Entity {
            record_ref: reference(reader, target),
        }
    } else {
        GraphObject::Literal {
            literal_type: row
                .record
                .string("wiki_literal_type")
                .unwrap_or_default()
                .into(),
            value: row
                .record
                .string("wiki_literal_value")
                .unwrap_or_default()
                .into(),
        }
    };
    Ok((
        reference(reader, subject),
        row.record
            .string("wiki_predicate")
            .unwrap_or_default()
            .into(),
        object,
        GraphQualifiers {
            negated: row
                .record
                .field("wiki_negated")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            modality: row
                .record
                .string("wiki_modality")
                .unwrap_or("asserted")
                .into(),
            property: row.record.string("wiki_property").map(str::to_owned),
            unit: row.record.string("wiki_unit").map(str::to_owned),
            valid_from: row.record.string("wiki_valid_from").map(str::to_owned),
            valid_until: row.record.string("wiki_valid_until").map(str::to_owned),
        },
    ))
}
fn usable(row: &RecordRow, scope: ContextScope) -> bool {
    row.eligibility != Eligibility::Invalid
        && match scope {
            ContextScope::Current => {
                row.eligibility == Eligibility::Current
                    && row.authored_status.as_deref() == Some("active")
            }
            _ => matches!(
                row.eligibility,
                Eligibility::Current | Eligibility::Historical | Eligibility::Withdrawn
            ),
        }
}
pub(super) fn owner(p: &ContextPassage) -> String {
    p.support_group
        .as_ref()
        .map(|h| format!("content:{h}"))
        .unwrap_or_else(|| {
            p.locator.record.as_ref().map_or_else(
                || format!("path:{}", p.locator.path),
                |r| format!("record:{}", r.record_id),
            )
        })
}
pub(super) fn merge(
    a: &mut ContextPassage,
    b: &ContextPassage,
    reader: &ReaderSnapshot,
) -> Result<bool> {
    if owner(a) != owner(b)
        || a.label != b.label
        || a.span.end() <= b.span.start()
        || b.span.end() <= a.span.start()
    {
        return Ok(false);
    }
    let start = a.span.start().min(b.span.start());
    let end = a.span.end().max(b.span.end());
    let document = reader
        .projection()
        .documents
        .iter()
        .find(|d| d.path == a.locator.path)
        .ok_or_else(|| WikiError::invalid("passage owner missing"))?;
    let span = ByteSpan::new(start, end)?;
    a.text = span.slice(&document.raw_text)?.to_owned();
    a.span = span;
    for c in &b.citations {
        if !a.citations.contains(c) {
            a.citations.push(c.clone())
        }
    }
    for c in &b.contributors {
        if !a.contributors.contains(c) {
            a.contributors.push(c.clone())
        }
    }
    for c in &b.rank_contributions {
        if !a.rank_contributions.contains(c) {
            a.rank_contributions.push(c.clone())
        }
    }
    Ok(true)
}
/// Close overlap components, including a late bridge across two older spans.
/// The map lets packing remap every already admitted bundle after coalescence.
pub(super) fn coalesce(
    passages: &mut Vec<ContextPassage>,
    reader: &ReaderSnapshot,
) -> Result<Vec<usize>> {
    let old = std::mem::take(passages);
    let length = old.len();
    let mut components: Vec<(Vec<usize>, ContextPassage)> = Vec::new();
    for (index, p) in old.into_iter().enumerate() {
        let mut current = p;
        let mut indices = vec![index];
        let mut at = 0;
        while at < components.len() {
            if merge(&mut components[at].1, &current, reader)? {
                let (members, merged) = components.remove(at);
                current = merged;
                indices.extend(members);
                at = 0;
            } else {
                at += 1
            }
        }
        components.push((indices, current));
    }
    components.sort_by_key(|(indices, _)| *indices.iter().min().expect("component member"));
    let mut mapping = vec![0; length];
    for (index, (members, p)) in components.into_iter().enumerate() {
        for old in members {
            mapping[old] = index
        }
        passages.push(p)
    }
    Ok(mapping)
}
pub(super) struct SelectedBundle {
    pub bundle: EvidenceBundle,
    pub passages: Vec<ContextPassage>,
}
pub(super) fn select(
    reader: &ReaderSnapshot,
    edge: &GraphAssertion,
    scope: ContextScope,
    rank: usize,
    source_ids: &[RecordId],
) -> Result<Option<SelectedBundle>> {
    let Some(row) = reader.projection().records.get(&edge.record_ref.record_id) else {
        return Ok(None);
    };
    if row.record.kind() != RecordKind::Assertion
        || row.eligibility == Eligibility::Invalid
        || row.authored_status.as_deref() == Some("proposed")
        || (scope == ContextScope::Current
            && (row.eligibility != Eligibility::Current
                || row.authored_status.as_deref() != Some("accepted")))
    {
        return Ok(None);
    }
    if edge.record_ref != reference(reader, row)
        || edge.locator.path != row.path
        || edge.locator.observed_hash != row.hash
    {
        return Err(WikiError::new(
            ErrorCode::FreshnessConflict,
            "graph locator disagrees with pinned canonical record",
        ));
    }
    let (subject, predicate, object, qualifiers) = proposition(reader, row)?;
    // Rebuild every path proposition; supplied descriptions, evidence and scores
    // never authorize a citation or change a canonical directed proposition.
    let mut path = Vec::new();
    let mut previous_end: Option<RecordId> = None;
    for (index, step) in edge.path.iter().enumerate() {
        let Some(record) = reader.projection().records.get(&step.assertion.record_id) else {
            return Err(WikiError::invalid("graph path record missing"));
        };
        if record.record.kind() != RecordKind::Assertion
            || record.eligibility == Eligibility::Invalid
            || record.authored_status.as_deref() == Some("proposed")
            || (scope == ContextScope::Current
                && (record.eligibility != Eligibility::Current
                    || record.authored_status.as_deref() != Some("accepted")))
        {
            return Ok(None);
        }
        let (s, p, o, q) = proposition(reader, record)?;
        if step.assertion != reference(reader, record)
            || step.subject != s
            || step.predicate != p
            || step.object != o
            || step.qualifiers != q
        {
            return Err(WikiError::invalid(
                "graph path disagrees with canonical proposition",
            ));
        }
        let (start, end) = match step.traversal {
            crate::graph::TraversalDirection::Outgoing => (
                Some(s.record_id.clone()),
                match &o {
                    GraphObject::Entity { record_ref } => Some(record_ref.record_id.clone()),
                    GraphObject::Literal { .. } => None,
                },
            ),
            crate::graph::TraversalDirection::Incoming => {
                let start = match &o {
                    GraphObject::Entity { record_ref } => Some(record_ref.record_id.clone()),
                    GraphObject::Literal { .. } if index == 0 => None,
                    _ => {
                        return Err(WikiError::invalid(
                            "literal cannot be an intermediate traversal node",
                        ));
                    }
                };
                (start, Some(s.record_id.clone()))
            }
        };
        if index > 0 && (previous_end.is_none() || previous_end != start) {
            return Err(WikiError::invalid(
                "graph path has disconnected directed steps",
            ));
        }
        previous_end = end;
        path.push(GraphPathStep {
            assertion: reference(reader, record),
            subject: s,
            predicate: p,
            object: o,
            qualifiers: q,
            traversal: step.traversal,
        });
    }
    if path.is_empty()
        || path
            .last()
            .is_none_or(|step| step.assertion.record_id != row.record.id().clone())
    {
        return Err(WikiError::invalid(
            "graph path does not terminate at selected assertion",
        ));
    }
    let contribution = RankContribution {
        channel: "graph_source_owner".into(),
        rank,
        score: None,
    };
    let mut passages: Vec<ContextPassage> = Vec::new();
    let mut totals = [0usize; 2];
    for evidence in reader.projection().records.values().filter(|r| {
        r.record.kind() == RecordKind::Evidence
            && r.record.string("wiki_assertion_id") == Some(row.record.id().as_str())
    }) {
        let stance = if evidence.record.string("wiki_stance") == Some("supports") {
            EvidenceStance::Supports
        } else {
            EvidenceStance::Contradicts
        };
        let index = usize::from(stance == EvidenceStance::Contradicts);
        totals[index] += 1;
        // Candidate assertion scope is broader than evidence scope. Preserve
        // full totals for honest omitted support/counterevidence disclosure.
        if !source_ids.is_empty()
            && !evidence
                .record
                .string("wiki_source_id")
                .is_some_and(|id| source_ids.iter().any(|source| source.as_str() == id))
        {
            continue;
        }
        if !usable(evidence, scope) {
            continue;
        }
        let source_id = RecordId::new(
            evidence
                .record
                .string("wiki_source_id")
                .expect("canonical evidence"),
        )?;
        let revision = RecordId::new(
            evidence
                .record
                .string("wiki_source_revision")
                .expect("canonical evidence"),
        )?;
        let Some(document) = reader.projection().documents.iter().find(|d| {
            d.source_id.as_ref() == Some(&source_id) && d.owner_revision.as_ref() == Some(&revision)
        }) else {
            continue;
        };
        let span = ByteSpan::new(
            evidence
                .record
                .field("wiki_span_start")
                .and_then(|v| v.as_u64())
                .expect("canonical span"),
            evidence
                .record
                .field("wiki_span_end")
                .and_then(|v| v.as_u64())
                .expect("canonical span"),
        )?;
        let text = span.slice(&document.raw_text)?.to_owned();
        let hash = Blake3Hash::new(
            evidence
                .record
                .string("wiki_quote_hash")
                .expect("canonical hash"),
        )?;
        if Blake3Hash::digest(text.as_bytes()) != hash {
            return Err(WikiError::new(
                ErrorCode::SourceIntegrity,
                "pinned evidence slice hash mismatch",
            ));
        }
        let reference = EvidenceRef {
            evidence_id: evidence.record.id().clone(),
            assertion_id: row.record.id().clone(),
            source_id,
            source_revision: revision.clone(),
            span,
            quote_hash: hash,
        };
        let p = ContextPassage {
            locator: DocumentLocator {
                record: Some(RecordRef {
                    vault_id: reader.projection().vault_id.clone(),
                    record_id: revision,
                    expected_kind: RecordKind::Revision,
                }),
                path: document.path.clone(),
                observed_hash: document.hash.clone(),
            },
            text,
            span,
            label: ExcerptLabel::CapturedSource,
            eligibility: evidence.eligibility,
            citations: if scope == ContextScope::Snapshot {
                vec![]
            } else {
                vec![CitationRef::Assertion(reference.clone())]
            },
            contributors: vec![EvidenceContribution {
                reference,
                stance,
                eligibility: evidence.eligibility,
                authored_status: evidence.authored_status.clone(),
            }],
            rank_contributions: vec![contribution.clone()],
            support_group: Some(document.hash.clone()),
        };
        let mut merged = false;
        for existing in &mut passages {
            if merge(existing, &p, reader)? {
                merged = true;
                break;
            }
        }
        if !merged {
            passages.push(p)
        }
    }
    coalesce(&mut passages, reader)?;
    passages.sort_by(|a, b| {
        usize::from(a.eligibility != Eligibility::Current)
            .cmp(&usize::from(b.eligibility != Eligibility::Current))
            .then(a.locator.path.cmp(&b.locator.path))
            .then(a.span.start().cmp(&b.span.start()))
    });
    let has = |p: &ContextPassage, stance| p.contributors.iter().any(|c| c.stance == stance);
    let mut selected = Vec::new();
    let mut support_groups = BTreeSet::new();
    // Reserve the required contradiction first. A mixed passage can satisfy
    // support as well, but cannot manufacture another independent support group.
    let contradiction = passages
        .iter()
        .position(|p| has(p, EvidenceStance::Contradicts) && has(p, EvidenceStance::Supports))
        .or_else(|| {
            passages
                .iter()
                .position(|p| has(p, EvidenceStance::Contradicts))
        });
    if let Some(index) = contradiction {
        let p = passages.remove(index);
        if has(&p, EvidenceStance::Supports) {
            support_groups.insert(owner(&p));
        }
        selected.push(p);
    }
    for p in passages {
        if !has(&p, EvidenceStance::Supports)
            || has(&p, EvidenceStance::Contradicts)
            || support_groups.len() >= 2
            || support_groups.contains(&owner(&p))
        {
            continue;
        }
        support_groups.insert(owner(&p));
        selected.push(p);
    }
    if support_groups.is_empty() {
        return Ok(None);
    }
    let mut retained = [BTreeSet::new(), BTreeSet::new()];
    for p in &selected {
        for c in &p.contributors {
            retained[usize::from(c.stance == EvidenceStance::Contradicts)]
                .insert(c.reference.evidence_id.clone());
        }
    }
    // An eligible contradiction is required even when a caller's discovery graph
    // set its evidence display cap to zero. Selection starts from the full registry.
    let bundle = EvidenceBundle {
        assertion: reference(reader, row),
        subject,
        predicate,
        object,
        qualifiers,
        authored_status: row.authored_status.clone(),
        eligibility: row.eligibility,
        disputed: row.disputed,
        path,
        passage_indices: vec![],
        omitted_support: totals[0].saturating_sub(retained[0].len()),
        omitted_contradictions: totals[1].saturating_sub(retained[1].len()),
        rank_contributions: vec![contribution],
    };
    Ok(Some(SelectedBundle {
        bundle,
        passages: selected,
    }))
}

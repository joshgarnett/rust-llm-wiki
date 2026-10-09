//! Complete explicit evidence reviews, guarded publication and authenticated reversal.
use super::receipt_budget::ReceiptBudget;
use super::{packet, remap, review_types::*};
use crate::{
    changes::*,
    domain::*,
    records::{ParsedNote, edit_note, parse_note},
    sources::{
        EvidenceStance, SourceView,
        evidence::{evidence_reference, note_quote},
        revision::{canonical_path, common, record_bytes, timestamp},
    },
    vault::{ExpectedState, VaultFs, WriterPermit},
};
use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[path = "review_receipt_v2.rs"]
pub(crate) mod receipt_v2;
pub(crate) use receipt_v2::{collect_review_receipts_v2, render_decisions_v2};

fn bad(s: impl Into<String>) -> WikiError {
    WikiError::invalid(s)
}
fn conflict(s: impl Into<String>) -> WikiError {
    WikiError::new(ErrorCode::ContentConflict, s)
}
fn budget() -> WikiError {
    WikiError::new(
        ErrorCode::BudgetExceeded,
        "review bounded capture exhausted",
    )
}
fn digest(b: Option<&[u8]>) -> ExpectedState {
    b.map_or(ExpectedState::Absent, |b| {
        ExpectedState::Hash(Blake3Hash::digest(b))
    })
}
fn record(n: &ParsedNote) -> Result<&CanonicalRecord> {
    n.canonical
        .as_ref()
        .ok_or_else(|| bad("review requires valid canonical records"))
}
fn find<'a>(
    notes: &'a BTreeMap<VaultRelativePath, ParsedNote>,
    id: &RecordId,
) -> Result<(&'a VaultRelativePath, &'a ParsedNote)> {
    remap::find(notes, id)
}
fn status(n: &ParsedNote) -> Result<&str> {
    record(n)?
        .string("wiki_status")
        .ok_or_else(|| bad("review status missing"))
}
fn idlist(r: &CanonicalRecord, key: &str) -> Result<Vec<RecordId>> {
    remap::id_list(r, key)
}
fn scope(r: &CanonicalRecord) -> Result<BTreeSet<RecordId>> {
    Ok(idlist(r, "wiki_input_ids")?
        .into_iter()
        .chain(idlist(r, "wiki_output_ids")?)
        .collect())
}
fn stance(r: &CanonicalRecord) -> Result<EvidenceStance> {
    match r.string("wiki_stance") {
        Some("supports") => Ok(EvidenceStance::Supports),
        Some("contradicts") => Ok(EvidenceStance::Contradicts),
        _ => Err(bad("unsupported evidence stance")),
    }
}
fn assessment(a: EvidenceAssessment) -> Option<EvidenceStance> {
    match a {
        EvidenceAssessment::Supports => Some(EvidenceStance::Supports),
        EvidenceAssessment::Contradicts => Some(EvidenceStance::Contradicts),
        EvidenceAssessment::Insufficient => None,
    }
}
fn changed(p: &ReviewEvidenceProof) -> bool {
    assessment(p.assessment).is_some_and(|s| s != p.before_stance)
}
fn assertion_status(s: &str) -> Result<ReviewedAssertionStatus> {
    match s {
        "proposed" => Ok(ReviewedAssertionStatus::Proposed),
        "accepted" => Ok(ReviewedAssertionStatus::Accepted),
        "rejected" => Ok(ReviewedAssertionStatus::Rejected),
        _ => Err(bad("superseded assertions cannot be reviewed")),
    }
}
fn as_status(s: ReviewedAssertionStatus) -> &'static str {
    match s {
        ReviewedAssertionStatus::Proposed => "proposed",
        ReviewedAssertionStatus::Accepted => "accepted",
        ReviewedAssertionStatus::Rejected => "rejected",
    }
}
fn e_status(s: ReviewedEvidenceStatus) -> &'static str {
    match s {
        ReviewedEvidenceStatus::Active => "active",
        ReviewedEvidenceStatus::Retracted => "retracted",
    }
}
fn path(kind: RecordKind, id: &RecordId) -> Result<VaultRelativePath> {
    if kind == RecordKind::Evidence {
        VaultRelativePath::new(format!("knowledge/evidence/{id}.md"))
    } else {
        super::decisions::path(kind, id)
    }
}
fn set_status(n: &ParsedNote, s: &str) -> Result<Vec<u8>> {
    edit_note(
        n,
        &BTreeMap::from([("wiki_status".into(), json!(s))]),
        None,
        &n.source_hash,
    )
}

pub(crate) fn normalize(mut r: ReviewRequest) -> Result<ReviewRequest> {
    if r.schema != GRAPH_REVIEW_SCHEMA
        || r.decisions.is_empty()
        || r.decisions.len() > MAX_REVIEWS
        || r.supersedes.len() > MAX_REVIEW_SUPERSEDES
    {
        return Err(bad("review schema or batch count invalid"));
    }
    let mut total = 0usize;
    let mut evidence = BTreeSet::new();
    for d in &mut r.decisions {
        if d.reason.trim().is_empty() || d.reason.len() > MAX_REVIEW_REASON_BYTES {
            return Err(bad("review rationale empty or too long"));
        }
        total = total
            .checked_add(d.evidence_checks.len())
            .ok_or_else(budget)?;
        if total > MAX_REVIEW_EVIDENCE {
            return Err(budget());
        }
        d.evidence_checks
            .sort_by(|a, b| a.evidence_id.cmp(&b.evidence_id));
        for e in &d.evidence_checks {
            if !evidence.insert(e.evidence_id.clone()) {
                return Err(bad("duplicate review evidence ID"));
            }
        }
    }
    r.decisions
        .sort_by(|a, b| a.assertion_id.cmp(&b.assertion_id));
    r.supersedes.sort_by(|a, b| a.record_id.cmp(&b.record_id));
    if r.decisions
        .windows(2)
        .any(|w| w[0].assertion_id == w[1].assertion_id)
        || r.supersedes
            .windows(2)
            .any(|w| w[0].record_id == w[1].record_id)
    {
        return Err(bad("duplicate review assertion/predecessor"));
    }
    if packet::canonical_json(&r)?.len() > MAX_REVIEW_BYTES {
        return Err(budget());
    }
    Ok(r)
}
fn identity(r: &ReviewRequest) -> Result<(RecordId, Blake3Hash)> {
    let assertions = r
        .decisions
        .iter()
        .map(|d| {
            let evidence = d
                .evidence_checks
                .iter()
                .map(|e| json!({"evidence_id":e.evidence_id,"expected_hash":e.expected_hash}))
                .collect::<Vec<_>>();
            json!({"assertion_id":d.assertion_id,"expected_hash":d.expected_hash,"evidence":evidence})
        })
        .collect::<Vec<_>>();
    // A generation includes every guarded input, while competing judgments about
    // that same generation share a scope and differ only in their request hash.
    let scope = Blake3Hash::digest(packet::canonical_json(&json!({
        "schema":"lwiki.graph-review-scope.v1",
        "assertions":assertions,
        "supersedes":r.supersedes,
    }))?);
    Ok((
        RecordId::new(format!(
            "review_{}",
            scope.as_str().trim_start_matches("blake3:")
        ))?,
        Blake3Hash::digest(packet::canonical_json(r)?),
    ))
}
fn proposition(r: &CanonicalRecord) -> Result<ReviewProposition> {
    if r.kind() != RecordKind::Assertion {
        return Err(bad("review target not assertion"));
    }
    let opt = |k: &str| r.string(k).map(str::to_owned);
    Ok(ReviewProposition {
        subject_id: RecordId::new(r.string("wiki_subject_id").expect("validated"))?,
        predicate: r.string("wiki_predicate").expect("validated").into(),
        object: match r.string("wiki_object_id") {
            Some(id) => ReviewObject::Entity {
                entity_id: RecordId::new(id)?,
            },
            None => ReviewObject::Literal {
                literal_type: r.string("wiki_literal_type").expect("validated").into(),
                literal_value: r.string("wiki_literal_value").expect("validated").into(),
            },
        },
        negated: r
            .field("wiki_negated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        modality: r.string("wiki_modality").unwrap_or("asserted").into(),
        property: opt("wiki_property"),
        unit: opt("wiki_unit"),
        valid_from: opt("wiki_valid_from"),
        valid_until: opt("wiki_valid_until"),
    })
}
// Semantic reconstruction only; never claimed as historical note bytes/before-image.
fn proposition_record(
    id: &RecordId,
    p: &ReviewProposition,
    status: &str,
) -> Result<CanonicalRecord> {
    let mut f = common(id, RecordKind::Assertion, "Historical reviewed proposition");
    f.extend(BTreeMap::from([
        ("wiki_status".into(), json!(status)),
        ("wiki_subject_id".into(), json!(p.subject_id)),
        ("wiki_predicate".into(), json!(p.predicate)),
        ("wiki_negated".into(), json!(p.negated)),
        ("wiki_modality".into(), json!(p.modality)),
    ]));
    match &p.object {
        ReviewObject::Entity { entity_id } => {
            f.insert("wiki_object_id".into(), json!(entity_id));
        }
        ReviewObject::Literal {
            literal_type,
            literal_value,
        } => {
            f.insert("wiki_literal_type".into(), json!(literal_type));
            f.insert("wiki_literal_value".into(), json!(literal_value));
        }
    }
    for (k, v) in [
        ("wiki_property", &p.property),
        ("wiki_unit", &p.unit),
        ("wiki_valid_from", &p.valid_from),
        ("wiki_valid_until", &p.valid_until),
    ] {
        if let Some(v) = v {
            f.insert(k.into(), json!(v));
        }
    }
    CanonicalRecord::new(f)
}
fn invariant(r: &CanonicalRecord) -> Result<Blake3Hash> {
    let mut f = BTreeMap::new();
    for k in [
        "wiki_kind",
        "wiki_assertion_id",
        "wiki_source_id",
        "wiki_source_revision",
        "wiki_locator_kind",
        "wiki_span_start",
        "wiki_span_end",
        "wiki_quote_hash",
        "wiki_extraction_id",
    ] {
        if let Some(v) = r.field(k) {
            f.insert(k, v);
        }
    }
    Ok(Blake3Hash::digest(packet::canonical_json(&f)?))
}
fn active(
    notes: &BTreeMap<VaultRelativePath, ParsedNote>,
    id: &RecordId,
) -> Result<BTreeMap<RecordId, Blake3Hash>> {
    let mut all = BTreeMap::new();
    for n in notes.values() {
        if let Some(r) = &n.canonical
            && r.kind() == RecordKind::Evidence
            && r.string("wiki_assertion_id") == Some(id.as_str())
            && r.string("wiki_status") == Some("active")
        {
            if all.insert(r.id().clone(), n.source_hash.clone()).is_some() {
                return Err(bad("duplicate active evidence identity"));
            }
            if all.len() > MAX_REVIEW_EVIDENCE {
                return Err(budget());
            }
        }
    }
    Ok(all)
}
fn predecessors(
    notes: &BTreeMap<VaultRelativePath, ParsedNote>,
    request: &ReviewRequest,
) -> Result<Vec<ReviewPredecessorProof>> {
    let targets = request
        .decisions
        .iter()
        .map(|d| d.assertion_id.clone())
        .collect::<BTreeSet<_>>();
    let declared = request
        .supersedes
        .iter()
        .map(|g| (g.record_id.clone(), g.hash.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut result = BTreeMap::new();
    for n in notes.values() {
        let Some(r) = &n.canonical else { continue };
        if r.kind() != RecordKind::Decision
            || r.string("wiki_status") != Some("active")
            || !matches!(r.string("wiki_action"), Some("accept" | "reject"))
        {
            continue;
        }
        let ids = scope(r)?;
        if !ids.is_disjoint(&targets) {
            if ids.is_empty()
                || !ids.is_subset(&targets)
                || declared.get(r.id()) != Some(&n.source_hash)
            {
                return Err(conflict(
                    "review requires exact full-scope active predecessor hashes",
                ));
            }
            for id in &ids {
                let (_, a) = find(notes, id)?;
                if record(a)?.kind() != RecordKind::Assertion {
                    return Err(bad("review predecessor scope not all assertions"));
                }
            }
            result.insert(
                r.id().clone(),
                ReviewPredecessorProof {
                    decision_id: r.id().clone(),
                    expected_hash: n.source_hash.clone(),
                    action: if r.string("wiki_action") == Some("accept") {
                        ReviewDecision::Accept
                    } else {
                        ReviewDecision::Reject
                    },
                    input_ids: idlist(r, "wiki_input_ids")?,
                    output_ids: idlist(r, "wiki_output_ids")?,
                },
            );
        }
    }
    if result.keys().ne(declared.keys()) {
        return Err(conflict(
            "review predecessor list missing, spurious or stale",
        ));
    }
    Ok(result.into_values().collect())
}

fn source_proof(
    view: &SourceView<'_>,
    n: &ParsedNote,
    deps: &mut BTreeMap<VaultRelativePath, ExpectedState>,
) -> Result<ReviewSourceProof> {
    let r = record(n)?;
    let e = evidence_reference(r)?;
    let (_, source) = view.resolve(&e.source_id, RecordKind::Source, r.string("wiki_source"))?;
    let (rp, rn) = view.resolve(
        &e.source_revision,
        RecordKind::Revision,
        r.string("wiki_revision"),
    )?;
    view.resolve(
        &e.assertion_id,
        RecordKind::Assertion,
        r.string("wiki_assertion"),
    )?;
    let sr = record(source)?;
    let rev = record(rn)?;
    if rev.string("wiki_source_id") != Some(e.source_id.as_str())
        || !idlist(sr, "wiki_revisions")?.contains(&e.source_revision)
    {
        return Err(bad("review revision ownership/retention mismatch"));
    }
    let head = RecordId::new(sr.string("wiki_current_revision").expect("validated"))?;
    let (_, hn) = view.resolve(&head, RecordKind::Revision, sr.string("wiki_revision"))?;
    if record(hn)?.string("wiki_source_id") != Some(e.source_id.as_str())
        || !idlist(sr, "wiki_revisions")?.contains(&head)
    {
        return Err(bad("review source head ownership mismatch"));
    }
    view.resolve(&e.source_id, RecordKind::Source, rev.string("wiki_source"))?;
    if rev.string("wiki_extraction_status") != Some("complete") {
        return Err(bad("review revision lacks extracted text"));
    }
    let parent = rp
        .as_str()
        .rsplit_once('/')
        .ok_or_else(|| bad("revision parent missing"))?
        .0;
    let original_path = VaultRelativePath::new(format!(
        "{parent}/{}",
        rev.string("wiki_original_path").expect("validated")
    ))?;
    let content_path = VaultRelativePath::new(format!(
        "{parent}/{}",
        rev.string("wiki_content_path").expect("validated")
    ))?;
    let used = deps
        .iter()
        .filter(|(p, _)| !view.notes.contains_key(*p))
        .try_fold(0usize, |s, (p, _)| {
            let len = if let Some(Some(b)) = view.overlay.get(p) {
                b.len()
            } else {
                usize::try_from(
                    std::fs::metadata(view.fs.root().resolve(p)?)
                        .map_err(|e| {
                            WikiError::new(
                                ErrorCode::Internal,
                                format!("inspect review source payload: {e}"),
                            )
                        })?
                        .len(),
                )
                .map_err(|_| budget())?
            };
            s.checked_add(len).ok_or_else(budget)
        })?;
    let mut remaining = MAX_REVIEW_CAPTURE_BYTES
        .checked_sub(used)
        .filter(|n| *n > 0)
        .ok_or_else(budget)?;
    let original_known = deps.contains_key(&original_path);
    let original = view.read_bounded(
        &original_path,
        deps,
        if original_known {
            MAX_REVIEW_CAPTURE_BYTES
        } else {
            remaining
        },
    )?;
    let oh = Blake3Hash::new(rev.string("wiki_original_hash").expect("validated"))?;
    if Blake3Hash::digest(&original) != oh {
        return Err(bad("review immutable original hash mismatch"));
    }
    if !original_known {
        remaining = remaining
            .checked_sub(original.len())
            .filter(|n| *n > 0)
            .ok_or_else(budget)?;
    }
    drop(original);
    let content_known = deps.contains_key(&content_path);
    let content = view.read_bounded(
        &content_path,
        deps,
        if content_known {
            MAX_REVIEW_CAPTURE_BYTES
        } else {
            remaining
        },
    )?;
    let ch = Blake3Hash::new(rev.string("wiki_content_hash").expect("validated"))?;
    let text = std::str::from_utf8(&content).map_err(|_| bad("review source not UTF8"))?;
    if Blake3Hash::digest(&content) != ch
        || e.span.is_empty()
        || Blake3Hash::digest(e.span.slice(text)?.as_bytes()) != e.quote_hash
        || note_quote(n)? != e.span.slice(text)?.as_bytes()
    {
        return Err(bad("review exact quotation/source proof mismatch"));
    }
    for (p, n) in view.notes.iter().filter(|(_, n)| {
        n.canonical.as_ref().is_some_and(|r| {
            r.id() == &e.source_id || r.id() == &e.source_revision || r.id() == &head
        })
    }) {
        SourceView::note_dependency(p, n, deps);
    }
    Ok(ReviewSourceProof {
        source_id: e.source_id,
        revision_id: e.source_revision,
        revision_hash: rn.source_hash.clone(),
        original_path,
        original_hash: oh,
        content_path,
        content_hash: ch,
        span: e.span,
        quote_hash: e.quote_hash,
    })
}
// Structural runtime-retained proof only. Full source bytes/quotation integrity and
// Current support are independently re-established by the same Catalog projection.
// This helper never opens an unprovided asset or presents metadata as source verification.
fn source_metadata(view: &SourceView<'_>, n: &ParsedNote) -> Result<ReviewSourceProof> {
    let r = record(n)?;
    let e = evidence_reference(r)?;
    let (_, sn) = view.resolve(&e.source_id, RecordKind::Source, r.string("wiki_source"))?;
    let (rp, rn) = view.resolve(
        &e.source_revision,
        RecordKind::Revision,
        r.string("wiki_revision"),
    )?;
    view.resolve(
        &e.assertion_id,
        RecordKind::Assertion,
        r.string("wiki_assertion"),
    )?;
    let source = record(sn)?;
    let revision = record(rn)?;
    let head = RecordId::new(source.string("wiki_current_revision").expect("validated"))?;
    let (_, hn) = view.resolve(&head, RecordKind::Revision, source.string("wiki_revision"))?;
    if revision.string("wiki_source_id") != Some(e.source_id.as_str())
        || record(hn)?.string("wiki_source_id") != Some(e.source_id.as_str())
        || !idlist(source, "wiki_revisions")?.contains(&e.source_revision)
        || !idlist(source, "wiki_revisions")?.contains(&head)
        || revision.string("wiki_extraction_status") != Some("complete")
    {
        return Err(bad(
            "review runtime source/revision ownership metadata differs",
        ));
    }
    view.resolve(
        &e.source_id,
        RecordKind::Source,
        revision.string("wiki_source"),
    )?;
    let parent = rp
        .as_str()
        .rsplit_once('/')
        .ok_or_else(|| bad("review revision parent missing"))?
        .0;
    if e.span.is_empty() || Blake3Hash::digest(note_quote(n)?) != e.quote_hash {
        return Err(bad("review runtime quote/span metadata differs"));
    }
    Ok(ReviewSourceProof {
        source_id: e.source_id,
        revision_id: e.source_revision,
        revision_hash: rn.source_hash.clone(),
        original_path: VaultRelativePath::new(format!(
            "{parent}/{}",
            revision.string("wiki_original_path").expect("validated")
        ))?,
        original_hash: Blake3Hash::new(revision.string("wiki_original_hash").expect("validated"))?,
        content_path: VaultRelativePath::new(format!(
            "{parent}/{}",
            revision.string("wiki_content_path").expect("complete")
        ))?,
        content_hash: Blake3Hash::new(revision.string("wiki_content_hash").expect("complete"))?,
        span: e.span,
        quote_hash: e.quote_hash,
    })
}
fn retained_semantics(view: &SourceView<'_>, r: &ReviewReceiptV1) -> Result<()> {
    if predecessors(&view.notes, &r.request)? != r.predecessors {
        return Err(bad("review runtime exact predecessor scope differs"));
    }
    for d in &r.request.decisions {
        let (_, n) = view.resolve(&d.assertion_id, RecordKind::Assertion, None)?;
        if n.source_hash != d.expected_hash
            || active(&view.notes, &d.assertion_id)?
                != d.evidence_checks
                    .iter()
                    .map(|c| (c.evidence_id.clone(), c.expected_hash.clone()))
                    .collect()
        {
            return Err(conflict(
                "review runtime complete logical-before membership/hash differs",
            ));
        }
    }
    for p in &r.evidence_proofs {
        let (_, n) = view.resolve(&p.evidence_id, RecordKind::Evidence, None)?;
        let er = record(n)?;
        if n.source_hash != check_for(r, &p.evidence_id)?.expected_hash
            || status(n)? != "active"
            || stance(er)? != p.before_stance
            || invariant(er)? != p.invariant_fields_hash
            || Blake3Hash::digest(n.body()) != p.body_hash
            || source_metadata(view, n)? != p.source
        {
            return Err(bad(
                "review runtime original evidence immutable trace/body differs",
            ));
        }
    }
    Ok(())
}
fn verify_fresh(
    view: &SourceView<'_>,
    request: &ReviewRequest,
) -> Result<(
    Vec<ReviewEvidenceProof>,
    BTreeMap<VaultRelativePath, ExpectedState>,
)> {
    predecessors(&view.notes, request)?;
    let mut proofs = vec![];
    let mut deps = BTreeMap::new();
    for d in &request.decisions {
        let (ap, an) = view.resolve(&d.assertion_id, RecordKind::Assertion, None)?;
        if an.source_hash != d.expected_hash {
            return Err(conflict("review assertion hash differs"));
        }
        assertion_status(status(an)?)?;
        SourceView::note_dependency(ap, an, &mut deps);
        if active(&view.notes, &d.assertion_id)?
            != d.evidence_checks
                .iter()
                .map(|c| (c.evidence_id.clone(), c.expected_hash.clone()))
                .collect()
        {
            return Err(conflict("review must cover every active evidence/hash"));
        }
        for c in &d.evidence_checks {
            let (ep, en) = view.resolve(&c.evidence_id, RecordKind::Evidence, None)?;
            let r = record(en)?;
            if en.source_hash != c.expected_hash
                || status(en)? != "active"
                || r.string("wiki_assertion_id") != Some(d.assertion_id.as_str())
            {
                return Err(conflict("review evidence hash/membership differs"));
            }
            SourceView::note_dependency(ep, en, &mut deps);
            let before_stance = stance(r)?;
            proofs.push(ReviewEvidenceProof {
                evidence_id: c.evidence_id.clone(),
                assertion_id: d.assertion_id.clone(),
                before_stance,
                assessment: c.assessment,
                after_status: if assessment(c.assessment).is_none_or(|s| s != before_stance) {
                    ReviewedEvidenceStatus::Retracted
                } else {
                    ReviewedEvidenceStatus::Active
                },
                source: source_proof(view, en, &mut deps)?,
                extraction_id: r
                    .string("wiki_extraction_id")
                    .map(RecordId::new)
                    .transpose()?,
                invariant_fields_hash: invariant(r)?,
                body_hash: Blake3Hash::digest(en.body()),
            });
        }
    }
    for d in &request.decisions {
        if d.decision == ReviewDecision::Accept {
            let supported = proofs
                .iter()
                .filter(|p| {
                    p.assertion_id == d.assertion_id && p.assessment == EvidenceAssessment::Supports
                })
                .any(|p| {
                    view.resolve(&p.source.source_id, RecordKind::Source, None)
                        .is_ok_and(|(_, n)| {
                            record(n).is_ok_and(|r| {
                                r.string("wiki_status") == Some("active")
                                    && r.string("wiki_current_revision")
                                        == Some(p.source.revision_id.as_str())
                            })
                        })
                });
            if !supported {
                return Err(bad(
                    "review acceptance requires post-assessment current supporting evidence",
                ));
            }
        }
    }
    Ok((proofs, deps))
}
fn receipt(n: &ParsedNote) -> Result<ReviewReceiptV1> {
    packet::decode(
        packet::fenced_json(n, GRAPH_REVIEW_FENCE, MAX_REVIEW_RECEIPT_BYTES)?,
        MAX_REVIEW_RECEIPT_BYTES,
    )
}
pub(crate) fn has_fence(n: &ParsedNote) -> bool {
    receipt_v2::witness_count(n) != 0
}
pub fn relevant_decision_ids(
    notes: &BTreeMap<VaultRelativePath, ParsedNote>,
) -> BTreeSet<RecordId> {
    notes
        .values()
        .filter(|n| has_fence(n))
        .filter_map(|n| {
            n.fields
                .as_ref()?
                .get("wiki_id")?
                .as_str()
                .and_then(|s| RecordId::new(s).ok())
        })
        .collect()
}
fn receipts(
    notes: &BTreeMap<VaultRelativePath, ParsedNote>,
) -> Result<BTreeMap<RecordId, ReviewReceiptV1>> {
    receipts_scoped(notes, &mut ReceiptBudget::default())
}
fn receipts_scoped(
    notes: &BTreeMap<VaultRelativePath, ParsedNote>,
    scope: &mut ReceiptBudget,
) -> Result<BTreeMap<RecordId, ReviewReceiptV1>> {
    Ok(collect_review_receipts_v2(notes, scope)?.receipts)
}
fn allocation_map(a: &ReviewAllocations) -> BTreeMap<String, RecordId> {
    a.decisions
        .iter()
        .map(|(old, new)| (format!("decision:{old}"), new.clone()))
        .chain(
            a.successors
                .iter()
                .map(|(old, new)| (format!("successor:{old}"), new.clone())),
        )
        .collect()
}
fn check_for<'a>(r: &'a ReviewReceiptV1, id: &RecordId) -> Result<&'a EvidenceCheck> {
    r.request
        .decisions
        .iter()
        .flat_map(|d| &d.evidence_checks)
        .find(|e| &e.evidence_id == id)
        .ok_or_else(|| bad("receipt evidence not requested"))
}
fn successor_bytes(r: &ReviewReceiptV1, p: &ReviewEvidenceProof) -> Result<Vec<u8>> {
    let n = parse_note(
        r.successor_templates
            .get(&p.evidence_id)
            .ok_or_else(|| bad("review successor template absent"))?
            .predecessor_note
            .as_bytes(),
    );
    let old = record(&n)?;
    if n.source_hash != check_for(r, &p.evidence_id)?.expected_hash
        || old.id() != &p.evidence_id
        || old.kind() != RecordKind::Evidence
        || status(&n)? != "active"
        || stance(old)? != p.before_stance
        || invariant(old)? != p.invariant_fields_hash
        || Blake3Hash::digest(n.body()) != p.body_hash
    {
        return Err(bad("review successor template not exact hashed original"));
    }
    let reference = evidence_reference(old)?;
    if reference.source_id != p.source.source_id
        || reference.source_revision != p.source.revision_id
        || reference.assertion_id != p.assertion_id
        || reference.span != p.source.span
        || reference.quote_hash != p.source.quote_hash
        || Blake3Hash::digest(note_quote(&n)?) != p.source.quote_hash
    {
        return Err(bad("review successor template immutable trace differs"));
    }
    let id = r
        .allocations
        .successors
        .get(&p.evidence_id)
        .ok_or_else(|| bad("review successor allocation absent"))?;
    let mut fields = old.fields().clone();
    fields.insert("wiki_id".into(), json!(id));
    fields.insert("wiki_status".into(), json!("active"));
    fields.insert(
        "wiki_stance".into(),
        json!(
            assessment(p.assessment)
                .ok_or_else(|| bad("insufficient cannot create successor"))?
                .as_str()
        ),
    );
    fields.insert("wiki_supersedes_id".into(), json!(p.evidence_id));
    for (k, id) in [
        ("wiki_assertion", &p.assertion_id),
        ("wiki_source", &p.source.source_id),
        ("wiki_revision", &p.source.revision_id),
        ("wiki_supersedes", &p.evidence_id),
    ] {
        fields.insert(
            k.into(),
            json!(format!(
                "[[{}]]",
                r.record_paths
                    .get(id)
                    .ok_or_else(|| bad("review original navigation path missing"))?
            )),
        );
    }
    let bytes = record_bytes(CanonicalRecord::new(fields)?, n.body())?;
    if n.newline == "\r\n" {
        let rendered = parse_note(&bytes);
        let mut output = Vec::with_capacity(bytes.len() + rendered.body_start);
        for b in &bytes[..rendered.body_start] {
            if *b == b'\n' {
                output.push(b'\r');
            }
            output.push(*b);
        }
        output.extend_from_slice(n.body());
        Ok(output)
    } else {
        Ok(bytes)
    }
}
fn decision_bytes(r: &ReviewReceiptV1, d: &AssertionReview) -> Result<Vec<u8>> {
    decision_with_proof(
        r,
        d,
        &packet::render_fence(r, GRAPH_REVIEW_FENCE, MAX_REVIEW_RECEIPT_BYTES)?,
    )
}
// The v1 envelope/body is shared verbatim with v2. Only the final proof fence
// differs; status remains mutable and is deliberately outside its commitment.
fn decision_with_proof(r: &ReviewReceiptV1, d: &AssertionReview, proof: &[u8]) -> Result<Vec<u8>> {
    let id = &r.allocations.decisions[&d.assertion_id];
    let mut f = common(
        id,
        RecordKind::Decision,
        "Explicit complete evidence review",
    );
    f.extend(BTreeMap::from([
        ("wiki_status".into(), json!("active")),
        ("wiki_action".into(), json!(d.decision.as_str())),
        ("wiki_created_at".into(), json!(r.created_at)),
        ("wiki_input_ids".into(), json!([d.assertion_id])),
        ("wiki_output_ids".into(), json!([d.assertion_id])),
    ]));
    let predecessors = r
        .supersessions
        .iter()
        .filter(|e| &e.successor_id == id)
        .map(|e| &e.predecessor_id)
        .collect::<Vec<_>>();
    if predecessors.len() == 1 {
        f.insert("wiki_supersedes_id".into(), json!(predecessors[0]));
        f.insert(
            "wiki_supersedes".into(),
            json!(format!("[[{}]]", r.record_paths[predecessors[0]])),
        );
    }
    let mut b = format!(
        "\n{}\n\nReviewed assertion [[{}]].\n",
        d.reason, r.record_paths[&d.assertion_id]
    )
    .into_bytes();
    b.extend_from_slice(proof);
    record_bytes(CanonicalRecord::new(f)?, &b)
}
fn edges(r: &ReviewReceiptV1) -> BTreeSet<(RecordId, RecordId)> {
    r.supersessions
        .iter()
        .map(|e| (e.predecessor_id.clone(), e.successor_id.clone()))
        .collect()
}
fn expected_edges(r: &ReviewReceiptV1) -> Result<BTreeSet<(RecordId, RecordId)>> {
    let mut all = BTreeSet::new();
    for p in &r.predecessors {
        let scope = p
            .input_ids
            .iter()
            .chain(&p.output_ids)
            .collect::<BTreeSet<_>>();
        if scope.is_empty() {
            return Err(bad("review predecessor empty scope"));
        }
        for id in scope {
            all.insert((
                p.decision_id.clone(),
                r.allocations
                    .decisions
                    .get(id)
                    .ok_or_else(|| bad("review predecessor scope not completely reviewed"))?
                    .clone(),
            ));
        }
    }
    Ok(all)
}
fn shape(r: &ReviewReceiptV1) -> Result<()> {
    if r.schema != GRAPH_REVIEW_RECEIPT_SCHEMA
        || normalize(r.request.clone())? != r.request
        || identity(&r.request)? != (r.task_id.clone(), r.request_hash.clone())
        || r.record_paths.len() > MAX_REVIEW_RECORD_PATHS
        || r.operations.len() > MAX_REVIEW_OPERATIONS
        || r.predecessors.len() > MAX_REVIEW_SUPERSEDES
    {
        return Err(bad("review receipt schema/identity/bounds invalid"));
    }
    let mut ids = BTreeSet::new();
    let expected_assert = r
        .request
        .decisions
        .iter()
        .map(|d| d.assertion_id.clone())
        .collect::<BTreeSet<_>>();
    if r.allocations
        .decisions
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>()
        != expected_assert
        || r.assertion_proofs.len() != r.request.decisions.len()
    {
        return Err(bad("review assertion/allocation keys differ"));
    }
    let mut required = expected_assert.clone();
    let mut seen_assert = BTreeSet::new();
    for (d, p) in r.request.decisions.iter().zip(&r.assertion_proofs) {
        if d.assertion_id != p.assertion_id
            || p.governing_decision_id != r.allocations.decisions[&d.assertion_id]
            || p.before_active_evidence
                != d.evidence_checks
                    .iter()
                    .map(|e| (e.evidence_id.clone(), e.expected_hash.clone()))
                    .collect()
            || !seen_assert.insert(p.assertion_id.clone())
        {
            return Err(bad("review complete assertion proof differs"));
        }
        proposition_record(&p.assertion_id, &p.proposition, as_status(p.before_status))?;
    }
    let checks = r
        .request
        .decisions
        .iter()
        .flat_map(|d| &d.evidence_checks)
        .map(|c| c.evidence_id.clone())
        .collect::<BTreeSet<_>>();
    if r.evidence_proofs.len() != checks.len()
        || r.evidence_proofs
            .iter()
            .map(|p| p.evidence_id.clone())
            .collect::<BTreeSet<_>>()
            != checks
    {
        return Err(bad("review exact evidence proof keys differ"));
    }
    let mut changed_ids = BTreeSet::new();
    for p in &r.evidence_proofs {
        let d = r
            .request
            .decisions
            .iter()
            .find(|d| d.assertion_id == p.assertion_id)
            .ok_or_else(|| bad("review evidence wrong assertion"))?;
        if d.evidence_checks
            .iter()
            .find(|c| c.evidence_id == p.evidence_id)
            .is_none_or(|c| c.assessment != p.assessment)
            || p.after_status
                != (if assessment(p.assessment).is_none_or(|s| s != p.before_stance) {
                    ReviewedEvidenceStatus::Retracted
                } else {
                    ReviewedEvidenceStatus::Active
                })
            || p.source.span.is_empty()
        {
            return Err(bad("review assessment/transition differs"));
        }
        required.extend([
            p.evidence_id.clone(),
            p.source.source_id.clone(),
            p.source.revision_id.clone(),
        ]);
        if changed(p) {
            changed_ids.insert(p.evidence_id.clone());
        }
    }
    if r.allocations
        .successors
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>()
        != changed_ids
        || r.successor_templates
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>()
            != changed_ids
    {
        return Err(bad("review successor/template exact keys differ"));
    }
    for id in allocation_map(&r.allocations).values() {
        if !ids.insert(id.clone()) || required.contains(id) {
            return Err(bad("review allocated identities overlap"));
        }
        required.insert(id.clone());
    }
    let declared = r
        .request
        .supersedes
        .iter()
        .map(|g| (g.record_id.clone(), g.hash.clone()))
        .collect::<BTreeMap<_, _>>();
    if r.predecessors.len() != declared.len()
        || r.predecessors
            .iter()
            .map(|p| (p.decision_id.clone(), p.expected_hash.clone()))
            .collect::<BTreeMap<_, _>>()
            != declared
    {
        return Err(bad("review predecessor proof keys/hashes differ"));
    }
    required.extend(declared.keys().cloned());
    if r.record_paths.keys().cloned().collect::<BTreeSet<_>>() != required
        || r.record_paths.values().collect::<BTreeSet<_>>().len() != r.record_paths.len()
    {
        return Err(bad("review original path exact keys/collision differs"));
    }
    if edges(r) != expected_edges(r)? || r.supersessions.len() != edges(r).len() {
        return Err(bad("review supersession full scope edges differ"));
    }
    if r.operations.windows(2).any(|w| w[0].target >= w[1].target) {
        return Err(bad("review write proof order or duplicates"));
    }
    for p in r.evidence_proofs.iter().filter(|p| changed(p)) {
        let bytes = successor_bytes(r, p)?;
        let id = &r.allocations.successors[&p.evidence_id];
        if !r.operations.iter().any(|w| {
            w.target == r.record_paths[id]
                && w.before == ExpectedState::Absent
                && w.after == digest(Some(&bytes))
        }) {
            return Err(bad("review successor original creation proof differs"));
        }
    }
    if packet::canonical_json(r)?.len() > MAX_REVIEW_RECEIPT_BYTES {
        return Err(budget());
    }
    Ok(())
}
#[derive(Debug)]
pub struct VerifiedReviewOverlay {
    accepted: BTreeSet<RecordId>,
    edges: BTreeSet<(RecordId, RecordId)>,
}
impl VerifiedReviewOverlay {
    pub fn accepted_assertions(&self) -> &BTreeSet<RecordId> {
        &self.accepted
    }
    pub fn supersession_edges(&self) -> &BTreeSet<(RecordId, RecordId)> {
        &self.edges
    }
}
#[derive(Debug)]
pub struct VerifiedReviewPolicy {
    edges: BTreeSet<(RecordId, RecordId)>,
}
impl VerifiedReviewPolicy {
    pub fn supersession_edges(&self) -> &BTreeSet<(RecordId, RecordId)> {
        &self.edges
    }
}
fn acyclic(edges: &BTreeSet<(RecordId, RecordId)>) -> Result<()> {
    acyclic_scoped(edges, &mut ReceiptBudget::default())
}
fn acyclic_scoped(edges: &BTreeSet<(RecordId, RecordId)>, scope: &mut ReceiptBudget) -> Result<()> {
    fn walk(
        id: &RecordId,
        edges: &BTreeMap<RecordId, BTreeSet<RecordId>>,
        visiting: &mut BTreeSet<RecordId>,
        memo: &mut BTreeMap<RecordId, usize>,
        scope: &mut ReceiptBudget,
    ) -> Result<usize> {
        scope.step()?;
        if let Some(n) = memo.get(id) {
            return Ok(*n);
        }
        if !visiting.insert(id.clone()) {
            return Err(bad("review supersession cycle"));
        }
        let mut depth = 0usize;
        for next in edges.get(id).into_iter().flatten() {
            depth = depth.max(
                walk(next, edges, visiting, memo, scope)?
                    .checked_add(1)
                    .ok_or_else(budget)?,
            );
            if depth > MAX_REVIEW_HISTORY_HOPS {
                return Err(bad("review history depth exceeds bound"));
            }
        }
        visiting.remove(id);
        memo.insert(id.clone(), depth);
        Ok(depth)
    }
    let mut graph = BTreeMap::<RecordId, BTreeSet<RecordId>>::new();
    for (a, b) in edges {
        scope.step()?;
        graph.entry(a.clone()).or_default().insert(b.clone());
    }
    let mut memo = BTreeMap::new();
    for id in graph.keys() {
        walk(id, &graph, &mut BTreeSet::new(), &mut memo, scope)?;
    }
    Ok(())
}
fn semantic_decision(
    r: &ReviewReceiptV1,
    d: &AssertionReview,
    n: &ParsedNote,
    v2: bool,
) -> Result<()> {
    // V2's complete family has already been authenticated once by the collector.
    // Do not serialize or clone the full carrier for each reference's envelope.
    let expected = parse_note(&decision_with_proof(r, d, &[])?);
    let got = record(n)?;
    let wanted = record(&expected)?;
    if got.kind() != RecordKind::Decision
        || got.id() != wanted.id()
        || got.string("wiki_action") != wanted.string("wiki_action")
        || got.field("wiki_input_ids") != wanted.field("wiki_input_ids")
        || got.field("wiki_output_ids") != wanted.field("wiki_output_ids")
        || got.field("wiki_created_at") != wanted.field("wiki_created_at")
        || got.field("wiki_supersedes_id") != wanted.field("wiki_supersedes_id")
        || (!v2 && receipt(n)? != *r)
    {
        return Err(bad("review Decision immutable envelope/proof differs"));
    }
    Ok(())
}
pub fn verify_review_policy(
    notes: &BTreeMap<VaultRelativePath, ParsedNote>,
) -> Result<Option<VerifiedReviewPolicy>> {
    verify_review_policy_scoped(notes, &mut ReceiptBudget::default())
}
pub(crate) fn verify_review_policy_for_vault(
    notes: &BTreeMap<VaultRelativePath, ParsedNote>,
    vault_id: &RecordId,
) -> Result<Option<VerifiedReviewPolicy>> {
    let mut scope = ReceiptBudget::default();
    scope.bind_vault(vault_id)?;
    verify_review_policy_scoped(notes, &mut scope)
}
pub(crate) fn verify_review_policy_scoped(
    notes: &BTreeMap<VaultRelativePath, ParsedNote>,
    scope: &mut ReceiptBudget,
) -> Result<Option<VerifiedReviewPolicy>> {
    let collected = collect_review_receipts_v2(notes, scope)?;
    if !collected.origins.is_empty() {
        let canonical_vault = notes
            .get(&VaultRelativePath::new("WIKI.md")?)
            .and_then(|note| note.canonical.as_ref())
            .filter(|record| record.kind() == RecordKind::Vault)
            .map(|record| record.id());
        if let Some(vault_id) = canonical_vault {
            scope.bind_vault(vault_id)?;
        }
        let actual_vault = scope
            .vault_id()
            .ok_or_else(|| bad("v2 review policy requires an authenticated vault identity"))?;
        collected.bind_vault(actual_vault)?;
    }
    let all = collected.receipts;
    if all.is_empty() {
        return Ok(None);
    }
    let mut combined = all.values().flat_map(edges).collect::<BTreeSet<_>>();
    if let Some(p) = remap::verify_decision_policy_scoped(notes, scope)? {
        combined.extend(p.supersession_edges().iter().cloned());
    }
    acyclic_scoped(&combined, scope)?;
    for r in all.values() {
        scope.step()?;
        for d in &r.request.decisions {
            scope.step()?;
            let (_, n) = scope.find(notes, &r.allocations.decisions[&d.assertion_id])?;
            semantic_decision(r, d, n, collected.origins.contains_key(&r.task_id))?;
            if status(n)? == "active" {
                let (_, an) = scope.find(notes, &d.assertion_id)?;
                if status(an)? != d.decision.assertion_status() {
                    return Err(bad("active review Decision outcome differs"));
                }
                if proposition(record(an)?)?
                    != r.assertion_proofs
                        .iter()
                        .find(|p| p.assertion_id == d.assertion_id)
                        .ok_or_else(|| bad("assertion proof missing"))?
                        .proposition
                {
                    return Err(bad("active review Decision proposition differs"));
                }
            } else if !combined
                .iter()
                .any(|(old, _)| old == &r.allocations.decisions[&d.assertion_id])
            {
                return Err(bad("review Decision superseded without explicit successor"));
            }
        }
        for p in &r.predecessors {
            scope.step()?;
            let (_, n) = scope.find(notes, &p.decision_id)?;
            let old = record(n)?;
            if old.kind() != RecordKind::Decision
                || status(n)? != "superseded"
                || old.string("wiki_action") != Some(p.action.as_str())
                || idlist(old, "wiki_input_ids")? != p.input_ids
                || idlist(old, "wiki_output_ids")? != p.output_ids
            {
                return Err(bad("review predecessor full scope/status differs"));
            }
        }
    }
    Ok(Some(VerifiedReviewPolicy {
        edges: all.values().flat_map(edges).collect(),
    }))
}
fn bounded(input: &ValidationInput) -> Result<()> {
    if input.documents.len() > MAX_REVIEW_CAPTURE_FILES
        || input.overlay.len() > MAX_REVIEW_OPERATIONS
        || input
            .documents
            .iter()
            .try_fold(0usize, |n, d| n.checked_add(d.bytes.len()))
            .is_none_or(|n| n > MAX_REVIEW_CAPTURE_BYTES)
        || input
            .overlay
            .iter()
            .filter_map(|o| o.bytes.as_ref())
            .try_fold(0usize, |n, b| n.checked_add(b.len()))
            .is_none_or(|n| n > MAX_REVIEW_CAPTURE_BYTES)
    {
        return Err(budget());
    }
    Ok(())
}
fn logical_input(
    input: &ValidationInput,
    w: Option<&RetainedGraphInput>,
) -> Result<ValidationInput> {
    let mut docs = input
        .documents
        .iter()
        .map(|d| (d.path.clone(), d.clone()))
        .collect::<BTreeMap<_, _>>();
    if docs.len() != input.documents.len() {
        return Err(bad("duplicate captured review paths"));
    }
    if let Some(w) = w {
        for op in w.operations() {
            if digest(op.before_bytes()) != *op.before() || digest(op.after_bytes()) != *op.after()
            {
                return Err(bad("review retained payload hashes differ"));
            }
            match op.before_bytes() {
                Some(b) => {
                    docs.insert(
                        op.path().clone(),
                        ScanDocument {
                            path: op.path().clone(),
                            bytes: b.to_vec(),
                            hash: Blake3Hash::digest(b),
                        },
                    );
                }
                None => {
                    docs.remove(op.path());
                }
            }
        }
    }
    let result = ValidationInput {
        vault_id: input.vault_id.clone(),
        documents: docs.into_values().collect(),
        overlay: vec![],
    };
    bounded(&result)?;
    Ok(result)
}
fn expected_writes(
    view: &SourceView<'_>,
    r: &ReviewReceiptV1,
) -> Result<BTreeMap<VaultRelativePath, ExpectedWrite>> {
    expected_writes_versioned(view, r, None)
}
pub(crate) fn expected_writes_v2(
    view: &SourceView<'_>,
    r: &ReviewReceiptV1,
    origin: &super::normalized_types::CanonicalGraphOriginV2,
) -> Result<BTreeMap<VaultRelativePath, ExpectedWrite>> {
    if origin.vault_id != packet::vault_id(view)? {
        return Err(bad("review origin belongs to another vault"));
    }
    expected_writes_versioned(view, r, Some(origin))
}
fn expected_writes_versioned(
    view: &SourceView<'_>,
    r: &ReviewReceiptV1,
    origin: Option<&super::normalized_types::CanonicalGraphOriginV2>,
) -> Result<BTreeMap<VaultRelativePath, ExpectedWrite>> {
    shape(r)?;
    let mut writes = BTreeMap::new();
    let mut add = |id: &RecordId, n: Option<&ParsedNote>, bytes: Vec<u8>| -> Result<()> {
        if n.is_some_and(|n| n.raw == bytes) {
            return Ok(());
        }
        let target = r
            .record_paths
            .get(id)
            .ok_or_else(|| bad("review original write path missing"))?
            .clone();
        if writes
            .insert(
                target.clone(),
                ExpectedWrite {
                    target,
                    expected: n.map_or(ExpectedState::Absent, |n| {
                        ExpectedState::Hash(n.source_hash.clone())
                    }),
                    proposed: Some(bytes),
                    apply_after: vec![],
                },
            )
            .is_some()
        {
            return Err(bad("review duplicate write target"));
        }
        Ok(())
    };
    for (d, p) in r.request.decisions.iter().zip(&r.assertion_proofs) {
        let (ap, an) = view.resolve(&d.assertion_id, RecordKind::Assertion, None)?;
        if ap != &r.record_paths[&d.assertion_id]
            || an.source_hash != d.expected_hash
            || assertion_status(status(an)?)? != p.before_status
            || proposition(record(an)?)? != p.proposition
        {
            return Err(bad("review assertion original write proof differs"));
        }
        add(
            &d.assertion_id,
            Some(an),
            set_status(an, d.decision.assertion_status())?,
        )?;
    }
    for p in &r.evidence_proofs {
        if p.after_status == ReviewedEvidenceStatus::Retracted {
            let (ep, en) = view.resolve(&p.evidence_id, RecordKind::Evidence, None)?;
            if ep != &r.record_paths[&p.evidence_id]
                || en.source_hash != check_for(r, &p.evidence_id)?.expected_hash
                || status(en)? != "active"
                || stance(record(en)?)? != p.before_stance
                || invariant(record(en)?)? != p.invariant_fields_hash
                || Blake3Hash::digest(en.body()) != p.body_hash
            {
                return Err(bad("review evidence original write proof differs"));
            }
            // Exact saved raw template is additionally required for changed stance.
            if changed(p)
                && r.successor_templates[&p.evidence_id]
                    .predecessor_note
                    .as_bytes()
                    != en.raw
            {
                return Err(bad(
                    "review successor template differs from actual before bytes",
                ));
            }
            add(&p.evidence_id, Some(en), set_status(en, "retracted")?)?;
        }
        if changed(p) {
            let id = &r.allocations.successors[&p.evidence_id];
            add(id, None, successor_bytes(r, p)?)?;
        }
    }
    for p in &r.predecessors {
        let (dp, dn) = view.resolve(&p.decision_id, RecordKind::Decision, None)?;
        let dr = record(dn)?;
        if dp != &r.record_paths[&p.decision_id]
            || dn.source_hash != p.expected_hash
            || status(dn)? != "active"
            || dr.string("wiki_action") != Some(p.action.as_str())
            || idlist(dr, "wiki_input_ids")? != p.input_ids
            || idlist(dr, "wiki_output_ids")? != p.output_ids
        {
            return Err(bad("review predecessor exact before proof differs"));
        }
        add(&p.decision_id, Some(dn), set_status(dn, "superseded")?)?;
    }
    let mut v2_decisions = origin
        .map(|origin| render_decisions_v2(r, origin))
        .transpose()?;
    for d in &r.request.decisions {
        let id = &r.allocations.decisions[&d.assertion_id];
        add(
            id,
            None,
            if let Some(rendered) = &mut v2_decisions {
                rendered
                    .remove(id)
                    .ok_or_else(|| bad("review rendered allocation missing"))?
            } else {
                decision_bytes(r, d)?
            },
        )?;
    }
    let nondecision = writes
        .values()
        .filter(|w| {
            parse_note(w.proposed.as_ref().expect("review writes bytes"))
                .canonical
                .as_ref()
                .is_some_and(|d| d.kind() != RecordKind::Decision)
        })
        .map(|w| ReviewWriteProof {
            target: w.target.clone(),
            before: w.expected.clone(),
            after: digest(w.proposed.as_deref()),
        })
        .collect::<Vec<_>>();
    if nondecision != r.operations {
        return Err(bad(
            "review exact non-Decision write proof membership differs",
        ));
    }
    Ok(writes)
}
pub fn verify_review_overlay(
    fs: &VaultFs,
    input: &ValidationInput,
    witness: Option<&RetainedGraphInput>,
) -> Result<Option<VerifiedReviewOverlay>> {
    // Inspect one canonical note at a time before constructing/cloning a closed
    // source view. Match its exact parse_note + Markdown fence semantics, rather
    // than searching raw text: info strings can contain escapes/entities.
    // Baseline notes count even when replaced/deleted, so removing a receipt
    // cannot make a potentially applicable review escape capture/authority checks.
    // Source payloads are not canonical receipts and never enter SourceView.notes.
    if witness.is_none()
        && !input.documents.iter().any(|document| {
            canonical_path(&document.path) && has_fence(&parse_note(&document.bytes))
        })
        && !input.overlay.iter().any(|target| {
            canonical_path(&target.path)
                && target
                    .bytes
                    .as_ref()
                    .is_some_and(|bytes| has_fence(&parse_note(bytes)))
        })
    {
        return Ok(None);
    }
    bounded(input)?;
    if witness.is_some_and(|w| w.origin().operation != OriginOperation::GraphReview) {
        return Ok(None);
    }
    let proposed = SourceView::from_closed_input(fs, input)?;
    let collected = match collect_review_receipts_v2(&proposed.notes, &mut ReceiptBudget::default())
    {
        Ok(r) => r,
        Err(e) => {
            if e.code == ErrorCode::BudgetExceeded {
                return Err(e);
            }
            if witness.is_none()
                && !input
                    .overlay
                    .iter()
                    .filter_map(|o| o.bytes.as_ref())
                    .map(|b| parse_note(b))
                    .any(|n| has_fence(&n))
            {
                return Ok(None);
            }
            return Err(e);
        }
    };
    collected.bind_vault(&input.vault_id)?;
    let all = &collected.receipts;
    let mut matched = vec![];
    for r in all.values() {
        if let Some(w) = witness {
            if r.task_id == w.origin().packet_id && r.request_hash == w.origin().response_hash {
                matched.push(r);
            }
        } else if r.allocations.decisions.values().any(|id| {
            !input.documents.iter().any(|d| {
                parse_note(&d.bytes)
                    .canonical
                    .as_ref()
                    .is_some_and(|old| old.id() == id)
            }) && input.overlay.iter().any(|o| {
                o.bytes.as_ref().is_some_and(|b| {
                    parse_note(b)
                        .canonical
                        .as_ref()
                        .is_some_and(|d| d.id() == id)
                })
            })
        }) {
            matched.push(r);
        }
    }
    if matched.is_empty() {
        if witness.is_some() {
            return Err(bad("GraphReview origin must match exactly one receipt"));
        }
        return Ok(None);
    }
    if matched.len() != 1 {
        return Err(bad("review overlay multiple authority receipts"));
    }
    let r = matched[0];
    if witness.is_some_and(|w| w.allocated_ids() != &allocation_map(&r.allocations)) {
        return Err(bad("GraphReview exact allocated ID map differs"));
    }
    let logical = logical_input(input, witness)?;
    let before = SourceView::from_closed_input(fs, &logical)?;
    if witness.is_some() {
        retained_semantics(&before, r)?;
    } else {
        let (proofs, _) = verify_fresh(&before, &r.request)?;
        if proofs != r.evidence_proofs || predecessors(&before.notes, &r.request)? != r.predecessors
        {
            return Err(bad(
                "review closed preparation complete source/predecessor proof differs",
            ));
        }
    }
    let expected = expected_writes_versioned(&before, r, collected.origins.get(&r.task_id))?;
    let overlays = input
        .overlay
        .iter()
        .map(|o| (&o.path, o.bytes.as_deref()))
        .collect::<BTreeMap<_, _>>();
    if overlays.len() != input.overlay.len()
        || overlays.keys().copied().collect::<BTreeSet<_>>()
            != expected.keys().collect::<BTreeSet<_>>()
    {
        return Err(bad("review exact operation set differs"));
    }
    let current = SourceView::from_closed_input(
        fs,
        &ValidationInput {
            vault_id: input.vault_id.clone(),
            documents: input.documents.clone(),
            overlay: vec![],
        },
    )?;
    for (p, w) in &expected {
        if overlays[p] != w.proposed.as_deref() {
            return Err(bad("review exact proposed bytes differ"));
        }
        let physical = current.notes.get(p).map(|n| n.raw.as_slice());
        if digest(physical) != w.expected && physical != w.proposed.as_deref() {
            return Err(conflict("review operation has unfamiliar current bytes"));
        }
    }
    if let Some(w) = witness {
        if w.operations().len() != expected.len() {
            return Err(bad("review witness has extra or missing operations"));
        }
        for op in w.operations() {
            let want = expected
                .get(op.path())
                .ok_or_else(|| bad("review witness extra write"))?;
            if op.role() != OperationRole::MutableRecord
                || op.before() != &want.expected
                || op.after() != &digest(want.proposed.as_deref())
                || op.after_bytes() != want.proposed.as_deref()
            {
                return Err(bad(
                    "review witness exact Decision/evidence write bytes differ",
                ));
            }
        }
    }
    for d in &r.request.decisions {
        let before_ids = d
            .evidence_checks
            .iter()
            .map(|c| c.evidence_id.clone())
            .collect::<BTreeSet<_>>();
        let mut final_ids = before_ids.clone();
        for p in r
            .evidence_proofs
            .iter()
            .filter(|p| p.assertion_id == d.assertion_id)
        {
            if p.after_status == ReviewedEvidenceStatus::Retracted {
                final_ids.remove(&p.evidence_id);
            }
            if let Some(id) = r.allocations.successors.get(&p.evidence_id) {
                final_ids.insert(id.clone());
            }
        }
        let now = active(&current.notes, &d.assertion_id)?
            .into_keys()
            .collect::<BTreeSet<_>>();
        let allowed = before_ids
            .union(&final_ids)
            .cloned()
            .collect::<BTreeSet<_>>();
        if !now.is_subset(&allowed)
            || active(&proposed.notes, &d.assertion_id)?
                .into_keys()
                .collect::<BTreeSet<_>>()
                != final_ids
        {
            return Err(conflict(
                "new active evidence or mismatched final membership during review",
            ));
        }
    }
    verify_review_policy(&proposed.notes)?
        .ok_or_else(|| bad("review proposed decision policy absent"))?;
    Ok(Some(VerifiedReviewOverlay {
        accepted: r
            .request
            .decisions
            .iter()
            .filter(|d| d.decision == ReviewDecision::Accept)
            .map(|d| d.assertion_id.clone())
            .collect(),
        edges: edges(r),
    }))
}
fn capture(
    view: &SourceView<'_>,
    extra: &BTreeMap<VaultRelativePath, ExpectedState>,
    source_ids: &BTreeSet<RecordId>,
) -> Result<(ValidationInput, Vec<ReadDependency>)> {
    let mut total = view
        .notes
        .values()
        .try_fold(0usize, |s, n| s.checked_add(n.raw.len()))
        .ok_or_else(budget)?;
    if total > MAX_REVIEW_CAPTURE_BYTES || view.notes.len() > MAX_REVIEW_CAPTURE_FILES {
        return Err(budget());
    }
    let mut docs = view
        .notes
        .iter()
        .map(|(p, n)| {
            (
                p.clone(),
                ScanDocument {
                    path: p.clone(),
                    bytes: n.raw.clone(),
                    hash: n.source_hash.clone(),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut deps = view
        .notes
        .iter()
        .map(|(p, n)| (p.clone(), ExpectedState::Hash(n.source_hash.clone())))
        .collect::<BTreeMap<_, _>>();
    for (p, expected) in extra {
        if let Some(old) = deps.get(p)
            && old != expected
        {
            return Err(conflict("review capture dependency changed"));
        }
        if !docs.contains_key(p) {
            let b = view.read_bounded(
                p,
                &mut deps,
                MAX_REVIEW_CAPTURE_BYTES
                    .checked_sub(total)
                    .filter(|n| *n > 0)
                    .ok_or_else(budget)?,
            )?;
            if digest(Some(&b)) != *expected {
                return Err(conflict(
                    "review source changed while capturing closed input",
                ));
            }
            total = total.checked_add(b.len()).ok_or_else(budget)?;
            docs.insert(
                p.clone(),
                ScanDocument {
                    path: p.clone(),
                    hash: Blake3Hash::digest(&b),
                    bytes: b,
                },
            );
        }
        deps.insert(p.clone(), expected.clone());
    }
    // The closed projection sees each selected source's entire revision manifest.
    // Include the payloads of its retained siblings, or an omitted sibling looks
    // corrupt and invalidates even evidence from the current revision.
    for source_id in source_ids {
        let (_, source_note) = view.resolve(source_id, RecordKind::Source, None)?;
        let source = record(source_note)?;
        for revision_id in idlist(source, "wiki_revisions")? {
            let (revision_path, revision_note) =
                view.resolve(&revision_id, RecordKind::Revision, None)?;
            let revision = record(revision_note)?;
            if revision.string("wiki_source_id") != Some(source_id.as_str()) {
                return Err(bad("review retained revision belongs to another source"));
            }
            let parent = revision_path
                .as_str()
                .rsplit_once('/')
                .map(|(parent, _)| parent)
                .ok_or_else(|| bad("review retained revision has no directory"))?;
            for field in ["wiki_original_path", "wiki_content_path"] {
                if let Some(name) = revision.string(field) {
                    let path = VaultRelativePath::new(format!("{parent}/{name}"))?;
                    if docs.contains_key(&path) {
                        continue;
                    }
                    if docs.len() >= MAX_REVIEW_CAPTURE_FILES {
                        return Err(budget());
                    }
                    let remaining = MAX_REVIEW_CAPTURE_BYTES
                        .checked_sub(total)
                        .filter(|n| *n > 0)
                        .ok_or_else(budget)?;
                    let bytes = view.read_bounded(&path, &mut deps, remaining)?;
                    total = total.checked_add(bytes.len()).ok_or_else(budget)?;
                    docs.insert(
                        path.clone(),
                        ScanDocument {
                            path,
                            hash: Blake3Hash::digest(&bytes),
                            bytes,
                        },
                    );
                }
            }
        }
    }
    Ok((
        ValidationInput {
            vault_id: packet::vault_id(view)?,
            documents: docs.into_values().collect(),
            overlay: vec![],
        },
        packet::dependencies(deps),
    ))
}
pub fn validate_review(view: &SourceView<'_>, bytes: &[u8]) -> Result<ValidatedReview> {
    let request = normalize(packet::decode(bytes, MAX_REVIEW_BYTES)?)?;
    let (task_id, request_hash) = identity(&request)?;
    if let Some(restored) = load_review_receipt(view, &task_id)? {
        if restored.receipt.request != request || restored.receipt.request_hash != request_hash {
            return Err(conflict(
                "different canonical review for guarded assertion scope",
            ));
        }
        let sources = restored
            .receipt
            .evidence_proofs
            .iter()
            .map(|proof| proof.source.source_id.clone())
            .collect();
        let (input, dependencies) = capture(
            view,
            &packet::dependency_map(&restored.dependencies)?,
            &sources,
        )?;
        return Ok(ValidatedReview {
            request,
            request_hash,
            task_id,
            dependencies,
            restored_receipt: Some(restored.receipt),
            input,
        });
    }
    let (proofs, deps) = verify_fresh(view, &request)?;
    let sources = proofs
        .iter()
        .map(|proof| proof.source.source_id.clone())
        .collect();
    let (input, dependencies) = capture(view, &deps, &sources)?;
    let closed = SourceView::from_closed_input(view.fs, &input)?;
    verify_fresh(&closed, &request)?;
    Ok(ValidatedReview {
        request,
        request_hash,
        task_id,
        dependencies,
        restored_receipt: None,
        input,
    })
}
fn summary(v: &ValidatedReview) -> Result<ReviewSummary> {
    let mut s = ReviewSummary {
        accept_assertions: v
            .request
            .decisions
            .iter()
            .filter(|d| d.decision == ReviewDecision::Accept)
            .count(),
        reject_assertions: v
            .request
            .decisions
            .iter()
            .filter(|d| d.decision == ReviewDecision::Reject)
            .count(),
        unchanged_evidence: 0,
        retract_evidence: 0,
        successor_evidence: 0,
        supersede_decisions: v.request.supersedes.len(),
        create_decisions: v.request.decisions.len(),
    };
    for c in v.request.decisions.iter().flat_map(|d| &d.evidence_checks) {
        let before = if let Some(r) = &v.restored_receipt {
            r.evidence_proofs
                .iter()
                .find(|p| p.evidence_id == c.evidence_id)
                .ok_or_else(|| bad("restored review summary missing evidence"))?
                .before_stance
        } else {
            let n = v
                .input
                .documents
                .iter()
                .map(|d| parse_note(&d.bytes))
                .find(|n| {
                    n.canonical
                        .as_ref()
                        .is_some_and(|r| r.id() == &c.evidence_id)
                })
                .ok_or_else(|| bad("review summary evidence unavailable"))?;
            stance(record(&n)?)?
        };
        if let Some(after) = assessment(c.assessment) {
            if after == before {
                s.unchanged_evidence += 1
            } else {
                s.retract_evidence += 1;
                s.successor_evidence += 1
            }
        } else {
            s.retract_evidence += 1
        }
    }
    Ok(s)
}
pub fn plan_review(v: &ValidatedReview) -> Result<ReviewPlan> {
    Ok(ReviewPlan {
        validated: v.clone(),
        summary: summary(v)?,
    })
}
fn build_draft(view: &SourceView<'_>, v: &ValidatedReview) -> Result<ChangeDraft> {
    Ok(build_draft_versioned(view, v, false)?.0)
}
/// Construct semantic writes from complete selected members. The sealed
/// selected projector owns graph closure/publication validation; a partial
/// selected view must never be sent to the legacy whole-graph validator.
pub(crate) fn build_draft_v2(
    view: &SourceView<'_>,
    v: &ValidatedReview,
) -> Result<(ChangeDraft, super::normalized_types::ReviewCarrierV2)> {
    let (draft, carrier) = build_draft_versioned(view, v, true)?;
    Ok((
        draft,
        carrier.ok_or_else(|| bad("review v2 carrier missing"))?,
    ))
}
fn build_draft_versioned(
    view: &SourceView<'_>,
    v: &ValidatedReview,
    v2: bool,
) -> Result<(
    ChangeDraft,
    Option<super::normalized_types::ReviewCarrierV2>,
)> {
    let (evidence_proofs, _) = verify_fresh(view, &v.request)?;
    let predecessors = predecessors(&view.notes, &v.request)?;
    let mut receipt = ReviewReceiptV1 {
        schema: GRAPH_REVIEW_RECEIPT_SCHEMA.into(),
        task_id: v.task_id.clone(),
        request: v.request.clone(),
        request_hash: v.request_hash.clone(),
        created_at: timestamp()?,
        allocations: ReviewAllocations {
            decisions: BTreeMap::new(),
            successors: BTreeMap::new(),
        },
        record_paths: BTreeMap::new(),
        assertion_proofs: vec![],
        evidence_proofs,
        successor_templates: BTreeMap::new(),
        predecessors,
        supersessions: vec![],
        operations: vec![],
    };
    // All original template sizes inspected before cloning captured author bytes.
    let mut template_total = 0usize;
    for p in receipt.evidence_proofs.iter().filter(|p| changed(p)) {
        let (_, n) = view.resolve(&p.evidence_id, RecordKind::Evidence, None)?;
        template_total = template_total.checked_add(n.raw.len()).ok_or_else(budget)?;
        if template_total > MAX_REVIEW_RECEIPT_BYTES {
            return Err(budget());
        }
    }
    for d in &v.request.decisions {
        let id = RecordId::generate(RecordKind::Decision)?;
        receipt
            .allocations
            .decisions
            .insert(d.assertion_id.clone(), id.clone());
        receipt
            .record_paths
            .insert(id.clone(), path(RecordKind::Decision, &id)?);
        let (ap, an) = view.resolve(&d.assertion_id, RecordKind::Assertion, None)?;
        receipt
            .record_paths
            .insert(d.assertion_id.clone(), ap.clone());
        receipt.assertion_proofs.push(ReviewAssertionProof {
            assertion_id: d.assertion_id.clone(),
            before_status: assertion_status(status(an)?)?,
            proposition: proposition(record(an)?)?,
            before_active_evidence: active(&view.notes, &d.assertion_id)?,
            governing_decision_id: id,
        });
    }
    for p in &receipt.evidence_proofs {
        for (id, kind) in [
            (&p.evidence_id, RecordKind::Evidence),
            (&p.source.source_id, RecordKind::Source),
            (&p.source.revision_id, RecordKind::Revision),
        ] {
            receipt
                .record_paths
                .insert(id.clone(), view.resolve(id, kind, None)?.0.clone());
        }
        if changed(p) {
            let id = RecordId::generate(RecordKind::Evidence)?;
            receipt
                .allocations
                .successors
                .insert(p.evidence_id.clone(), id.clone());
            receipt
                .record_paths
                .insert(id.clone(), path(RecordKind::Evidence, &id)?);
            let (_, n) = view.resolve(&p.evidence_id, RecordKind::Evidence, None)?;
            receipt.successor_templates.insert(
                p.evidence_id.clone(),
                ReviewSuccessorTemplate {
                    predecessor_note: String::from_utf8(n.raw.clone())
                        .map_err(|_| bad("review predecessor not UTF8"))?,
                },
            );
        }
    }
    for p in &receipt.predecessors {
        receipt.record_paths.insert(
            p.decision_id.clone(),
            view.resolve(&p.decision_id, RecordKind::Decision, None)?
                .0
                .clone(),
        );
    }
    receipt.supersessions = expected_edges(&receipt)?
        .into_iter()
        .map(|(predecessor_id, successor_id)| ReviewSupersession {
            predecessor_id,
            successor_id,
        })
        .collect();
    let mut writes = BTreeMap::new();
    for d in &receipt.request.decisions {
        let (p, n) = view.resolve(&d.assertion_id, RecordKind::Assertion, None)?;
        writes.insert(
            p.clone(),
            ExpectedWrite {
                target: p.clone(),
                expected: ExpectedState::Hash(n.source_hash.clone()),
                proposed: Some(set_status(n, d.decision.assertion_status())?),
                apply_after: vec![],
            },
        );
    }
    for p in &receipt.evidence_proofs {
        if p.after_status == ReviewedEvidenceStatus::Retracted {
            let (ep, n) = view.resolve(&p.evidence_id, RecordKind::Evidence, None)?;
            writes.insert(
                ep.clone(),
                ExpectedWrite {
                    target: ep.clone(),
                    expected: ExpectedState::Hash(n.source_hash.clone()),
                    proposed: Some(set_status(n, "retracted")?),
                    apply_after: vec![],
                },
            );
        }
        if changed(p) {
            let id = &receipt.allocations.successors[&p.evidence_id];
            let target = receipt.record_paths[id].clone();
            writes.insert(
                target.clone(),
                ExpectedWrite {
                    target,
                    expected: ExpectedState::Absent,
                    proposed: Some(successor_bytes(&receipt, p)?),
                    apply_after: vec![],
                },
            );
        }
    }
    writes.retain(|_, w| {
        w.proposed
            .as_deref()
            .is_none_or(|b| digest(Some(b)) != w.expected)
    });
    receipt.operations = writes
        .values()
        .map(|w| ReviewWriteProof {
            target: w.target.clone(),
            before: w.expected.clone(),
            after: digest(w.proposed.as_deref()),
        })
        .collect();
    shape(&receipt)?;
    for p in &receipt.predecessors {
        let (dp, n) = view.resolve(&p.decision_id, RecordKind::Decision, None)?;
        writes.insert(
            dp.clone(),
            ExpectedWrite {
                target: dp.clone(),
                expected: ExpectedState::Hash(n.source_hash.clone()),
                proposed: Some(set_status(n, "superseded")?),
                apply_after: vec![],
            },
        );
    }
    let carrier = if v2 {
        let origin = super::normalized_types::CanonicalGraphOriginV2::for_task(
            packet::vault_id(view)?,
            super::normalized_types::GraphOperationFamily::AssertionReview,
            &receipt.task_id,
            Blake3Hash::digest(packet::canonical_json(&receipt)?),
        )?;
        Some(receipt_v2::carrier(&receipt, &origin)?)
    } else {
        None
    };
    let mut rendered_v2 = carrier
        .as_ref()
        .map(receipt_v2::render_carrier_decisions)
        .transpose()?;
    for d in &receipt.request.decisions {
        let target = receipt.record_paths[&receipt.allocations.decisions[&d.assertion_id]].clone();
        writes.insert(
            target.clone(),
            ExpectedWrite {
                target,
                expected: ExpectedState::Absent,
                proposed: Some(if let Some(rendered) = &mut rendered_v2 {
                    rendered
                        .remove(&receipt.allocations.decisions[&d.assertion_id])
                        .ok_or_else(|| bad("review rendered allocation missing"))?
                } else {
                    decision_bytes(&receipt, d)?
                }),
                apply_after: vec![],
            },
        );
    }
    if !v2 {
        let input = ValidationInput {
            vault_id: v.input.vault_id.clone(),
            documents: v.input.documents.clone(),
            overlay: writes
                .values()
                .map(|w| ProposedTarget {
                    path: w.target.clone(),
                    bytes: w.proposed.clone(),
                })
                .collect(),
        };
        bounded(&input)?;
        verify_review_overlay(view.fs, &input, None)?
            .ok_or_else(|| bad("review draft lacks exact authority"))?;
        crate::catalog::CatalogGraphValidator.validate_closed(view.fs, &input)?;
    }
    Ok((
        ChangeDraft {
            title: "Explicit complete evidence review".into(),
            origin: None,
            inverse_of: None,
            allocated_ids: allocation_map(&receipt.allocations),
            read_preconditions: v.dependencies.clone(),
            operations: writes.into_values().collect(),
        },
        carrier,
    ))
}
fn evidence_semantics(
    view: &SourceView<'_>,
    r: &ReviewReceiptV1,
    p: &ReviewEvidenceProof,
    deps: &mut BTreeMap<VaultRelativePath, ExpectedState>,
    all: &BTreeMap<RecordId, ReviewReceiptV1>,
) -> Result<()> {
    let (_, n) = view.resolve(&p.evidence_id, RecordKind::Evidence, None)?;
    let rec = record(n)?;
    if stance(rec)? != p.before_stance
        || invariant(rec)? != p.invariant_fields_hash
        || source_proof(view, n, deps)? != p.source
    {
        return Err(bad("original reviewed evidence immutable trace differs"));
    }
    if status(n)? != e_status(p.after_status)
        && (p.after_status != ReviewedEvidenceStatus::Active
            || status(n)? != "retracted"
            || !all.values().any(|next| {
                next.task_id != r.task_id
                    && next.evidence_proofs.iter().any(|np| {
                        np.evidence_id == p.evidence_id
                            && np.before_stance == p.before_stance
                            && np.source == p.source
                            && np.invariant_fields_hash == p.invariant_fields_hash
                            && np.after_status == ReviewedEvidenceStatus::Retracted
                    })
            }))
    {
        return Err(bad("evidence status changed without explicit later review"));
    }
    if let Some(id) = r.allocations.successors.get(&p.evidence_id) {
        let (_, sn) = view.resolve(id, RecordKind::Evidence, None)?;
        let want = parse_note(&successor_bytes(r, p)?);
        let sr = record(sn)?;
        if invariant(sr)? != invariant(record(&want)?)?
            || stance(sr)?
                != assessment(p.assessment).ok_or_else(|| bad("invalid successor assessment"))?
            || sr.string("wiki_supersedes_id") != Some(p.evidence_id.as_str())
            || source_proof(view, sn, deps)? != p.source
        {
            return Err(bad("review successor immutable quotation/stance differs"));
        }
        if status(sn)? != "active"
            && !all.values().any(|next| {
                next.task_id != r.task_id
                    && next.evidence_proofs.iter().any(|np| {
                        np.evidence_id == *id
                            && np.after_status == ReviewedEvidenceStatus::Retracted
                            && np.source == p.source
                            && np.before_stance == stance(sr).unwrap_or(p.before_stance)
                    })
            })
        {
            return Err(bad("review successor retraction has no explicit proof"));
        }
    }
    Ok(())
}
pub fn verify_review_evolution(
    view: &SourceView<'_>,
    assertion_id: &RecordId,
    r: &ReviewReceiptV1,
) -> Result<Vec<ReadDependency>> {
    let collected = collect_review_receipts_v2(&view.notes, &mut ReceiptBudget::default())?;
    if !collected.origins.is_empty() {
        collected.bind_vault(&packet::vault_id(view)?)?;
    }
    verify_review_evolution_collected(view, assertion_id, r, &collected.receipts)?;
    Ok(view
        .notes
        .iter()
        .map(|(p, n)| ReadDependency {
            path: p.clone(),
            expected: ExpectedState::Hash(n.source_hash.clone()),
        })
        .collect())
}
fn verify_review_evolution_collected(
    view: &SourceView<'_>,
    assertion_id: &RecordId,
    r: &ReviewReceiptV1,
    all: &BTreeMap<RecordId, ReviewReceiptV1>,
) -> Result<()> {
    let p = r
        .assertion_proofs
        .iter()
        .find(|p| &p.assertion_id == assertion_id)
        .ok_or_else(|| bad("review evolution assertion not in receipt"))?;
    let (_, n) = view.resolve(assertion_id, RecordKind::Assertion, None)?;
    let current = record(n)?;
    let old = proposition_record(assertion_id, &p.proposition, "proposed")?;
    if proposition(current)? != p.proposition {
        remap::verify_proposition_evolution(view, assertion_id, &old, current)?;
    }
    let decision = &r.allocations.decisions[assertion_id];
    let (_, dn) = view.resolve(decision, RecordKind::Decision, None)?;
    if status(dn)? == "active" {
        let d = r
            .request
            .decisions
            .iter()
            .find(|d| &d.assertion_id == assertion_id)
            .expect("shape verified");
        if status(n)? != d.decision.assertion_status() || proposition(current)? != p.proposition {
            return Err(bad("active review current outcome not exact"));
        }
    } else {
        let mut combined = all.values().flat_map(edges).collect::<BTreeSet<_>>();
        if let Some(policy) = remap::verify_decision_policy(&view.notes)? {
            combined.extend(policy.supersession_edges().iter().cloned());
        }
        acyclic(&combined)?;
        let mut reachable = BTreeSet::from([decision.clone()]);
        for _ in 0..MAX_REVIEW_HISTORY_HOPS {
            let next = combined
                .iter()
                .filter(|(a, _)| reachable.contains(a))
                .map(|(_, b)| b.clone())
                .collect::<BTreeSet<_>>();
            let old_len = reachable.len();
            reachable.extend(next);
            if old_len == reachable.len() {
                break;
            }
        }
        let desired = |next: &ReviewReceiptV1| -> bool {
            next.request.decisions.iter().any(|d| {
                &d.assertion_id == assertion_id
                    && d.decision.assertion_status() == current.string("wiki_status").unwrap_or("")
                    && next.assertion_proofs.iter().any(|np| {
                        np.assertion_id == *assertion_id
                            && proposition(current).is_ok_and(|cp| cp == np.proposition)
                    })
                    && view
                        .resolve(
                            &next.allocations.decisions[assertion_id],
                            RecordKind::Decision,
                            None,
                        )
                        .is_ok_and(|(_, dn)| status(dn).is_ok_and(|s| s == "active"))
            })
        };
        let direct = all.values().any(|next| {
            next.allocations
                .decisions
                .get(assertion_id)
                .is_some_and(|id| reachable.contains(id))
                && desired(next)
        });
        // A fresh review after P12 has no identity-main predecessor field. Bridge only
        // the saved exact P12 assertion AFTER hash to a later review BEFORE hash.
        let mut hashes = BTreeSet::new();
        let mut remapped = false;
        for n in view.notes.values() {
            if let Some(d) = &n.canonical
                && d.kind() == RecordKind::Decision
                && reachable.contains(d.id())
                && matches!(d.string("wiki_action"), Some("merge" | "split"))
            {
                let receipt = remap::receipt(n)?;
                if receipt
                    .assertion_proofs
                    .iter()
                    .any(|p| &p.assertion_id == assertion_id)
                {
                    let path = receipt
                        .record_paths
                        .get(assertion_id)
                        .ok_or_else(|| bad("historical remap assertion path missing"))?;
                    for op in &receipt.operations {
                        if &op.target == path
                            && let ExpectedState::Hash(h) = &op.after
                        {
                            hashes.insert(h.clone());
                        }
                    }
                    remapped = true;
                }
            }
        }
        let mut bridged = false;
        for _ in 0..MAX_REVIEW_HISTORY_HOPS {
            let previous = hashes.len();
            for next in all.values() {
                if let Some(d) = next
                    .request
                    .decisions
                    .iter()
                    .find(|d| &d.assertion_id == assertion_id)
                    && hashes.contains(&d.expected_hash)
                {
                    if desired(next) {
                        bridged = true;
                    }
                    let path = next
                        .record_paths
                        .get(assertion_id)
                        .ok_or_else(|| bad("historical review assertion path missing"))?;
                    for op in &next.operations {
                        if &op.target == path
                            && let ExpectedState::Hash(h) = &op.after
                        {
                            hashes.insert(h.clone());
                        }
                    }
                }
            }
            if hashes.len() == previous {
                break;
            }
        }
        if !direct
            && !bridged
            && !(remapped && status(n)? == "proposed" && proposition(current)? != p.proposition)
        {
            return Err(bad(
                "historical review has no reachable explicit Decision/hash descendant",
            ));
        }
    }
    Ok(())
}
pub fn load_review_receipt(
    view: &SourceView<'_>,
    task_id: &RecordId,
) -> Result<Option<VerifiedReviewReceipt>> {
    let collected = collect_review_receipts_v2(&view.notes, &mut ReceiptBudget::default())?;
    if !collected.origins.is_empty() {
        collected.bind_vault(&packet::vault_id(view)?)?;
    }
    let all = &collected.receipts;
    let Some(r) = all.get(task_id) else {
        return Ok(None);
    };
    verify_review_policy(&view.notes)?.ok_or_else(|| bad("canonical review policy absent"))?;
    let mut deps = view
        .notes
        .iter()
        .map(|(p, n)| (p.clone(), ExpectedState::Hash(n.source_hash.clone())))
        .collect::<BTreeMap<_, _>>();
    let mut decision_locators = vec![];
    for d in &r.request.decisions {
        verify_review_evolution_collected(view, &d.assertion_id, r, all)?;
        let (p, n) = view.resolve(
            &r.allocations.decisions[&d.assertion_id],
            RecordKind::Decision,
            None,
        )?;
        decision_locators.push(packet::locator(view, p, n)?);
    }
    for p in &r.evidence_proofs {
        evidence_semantics(view, r, p, &mut deps, all)?;
    }
    Ok(Some(VerifiedReviewReceipt {
        receipt: r.clone(),
        dependencies: packet::dependencies(deps),
        decision_locators,
    }))
}
fn retained_receipt(
    engine: &ChangeEngine,
    v: &ValidatedReview,
    c: &ChangeInspection,
    known: Option<&ReviewReceiptV1>,
) -> Result<VerifiedReviewReceipt> {
    let mut base = v
        .input
        .documents
        .iter()
        .map(|d| (d.path.clone(), d.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut overlay = vec![];
    let mut total = 0usize;
    for (i, op) in c.manifest.operations.iter().enumerate() {
        for payload in [&op.before_payload, &op.after_payload]
            .into_iter()
            .flatten()
        {
            total = total
                .checked_add(usize::try_from(payload.byte_len).map_err(|_| budget())?)
                .ok_or_else(budget)?;
            if total > MAX_REVIEW_RETAINED_BYTES {
                return Err(budget());
            }
        }
        let before = engine.verify_payload(
            &c.manifest.change_id,
            i,
            "before",
            &op.target,
            &op.before,
            &op.before_payload,
        )?;
        match before {
            Some(bytes) => {
                base.insert(
                    op.target.clone(),
                    ScanDocument {
                        path: op.target.clone(),
                        hash: Blake3Hash::digest(&bytes),
                        bytes,
                    },
                );
            }
            None => {
                base.remove(&op.target);
            }
        }
        overlay.push(ProposedTarget {
            path: op.target.clone(),
            bytes: engine.verify_payload(
                &c.manifest.change_id,
                i,
                "proposed",
                &op.target,
                &op.after,
                &op.after_payload,
            )?,
        });
    }
    let before_input = ValidationInput {
        vault_id: v.input.vault_id.clone(),
        documents: base.into_values().collect(),
        overlay: vec![],
    };
    bounded(&before_input)?;
    let proposal = ValidationInput {
        vault_id: before_input.vault_id.clone(),
        documents: before_input.documents.clone(),
        overlay,
    };
    bounded(&proposal)?;
    let view = SourceView::from_closed_input(engine.fs(), &proposal)?;
    let collected = collect_review_receipts_v2(&view.notes, &mut ReceiptBudget::default())?;
    collected.bind_vault(&proposal.vault_id)?;
    let proof = if let Some(r) = known {
        // Actual canonical descendant was fully validated separately. Authenticate
        // old receipt copies from exact retained proposed Decision bytes without
        // injecting old active authority into today's later reviewed graph.
        if collected.receipts.get(&r.task_id) != Some(r) {
            return Err(bad(
                "retained original receipt differs from canonical immutable allocation/proof maps",
            ));
        }
        for d in &r.request.decisions {
            let (_, n) = view.resolve(
                &r.allocations.decisions[&d.assertion_id],
                RecordKind::Decision,
                None,
            )?;
            semantic_decision(r, d, n, collected.origins.contains_key(&r.task_id))?;
        }
        VerifiedReviewReceipt {
            receipt: r.clone(),
            dependencies: vec![],
            decision_locators: vec![],
        }
    } else {
        load_review_receipt(&view, &v.task_id)?
            .ok_or_else(|| bad("retained review receipt absent"))?
    };
    if known.is_some_and(|r| r != &proof.receipt)
        || proof.receipt.request != v.request
        || proof.receipt.request_hash != v.request_hash
        || c.manifest.origin.as_ref()
            != Some(&ChangeOrigin {
                operation: OriginOperation::GraphReview,
                packet_id: v.task_id.clone(),
                response_hash: v.request_hash.clone(),
            })
        || c.manifest.allocated_ids != allocation_map(&proof.receipt.allocations)
    {
        return Err(bad(
            "retained/canonical review exact origin/request/allocation differs",
        ));
    }
    let before = SourceView::from_closed_input(engine.fs(), &before_input)?;
    let expected = expected_writes_versioned(
        &before,
        &proof.receipt,
        collected.origins.get(&proof.receipt.task_id),
    )?;
    if c.manifest.operations.len() != expected.len() {
        return Err(bad("retained review exact Decision/write count differs"));
    }
    for (op, target) in c.manifest.operations.iter().zip(&proposal.overlay) {
        let w = expected
            .get(&op.target)
            .ok_or_else(|| bad("retained review extra operation"))?;
        if op.role != OperationRole::MutableRecord
            || op.before != w.expected
            || op.after != digest(w.proposed.as_deref())
            || target.bytes != w.proposed
        {
            return Err(bad("retained review original exact operation bytes differ"));
        }
    }
    Ok(proof)
}
pub fn stage_review(
    engine: &ChangeEngine,
    writer: &WriterPermit,
    v: &ValidatedReview,
) -> Result<ReviewOutcome> {
    writer.require_root(engine.fs().root())?;
    let physical = SourceView::from_fs_bounded(
        engine.fs(),
        MAX_REVIEW_CAPTURE_BYTES,
        MAX_REVIEW_CAPTURE_FILES,
    )?;
    let current = validate_review(&physical, &packet::canonical_json(&v.request)?)?;
    let origin = ChangeOrigin {
        operation: OriginOperation::GraphReview,
        packet_id: v.task_id.clone(),
        response_hash: v.request_hash.clone(),
    };
    if let Some(r) = &current.restored_receipt {
        let mut retained = None;
        for id in engine.change_ids()? {
            let c = engine.inspect_history(&id)?;
            if c.manifest
                .origin
                .as_ref()
                .is_some_and(|o| o.operation == origin.operation && o.packet_id == origin.packet_id)
            {
                if c.manifest.origin.as_ref() != Some(&origin) {
                    return Err(conflict(
                        "different retained review for canonical guarded scope",
                    ));
                }
                retained_receipt(engine, &current, &c, Some(r))?;
                if retained.replace(c).is_some() {
                    return Err(bad("duplicate retained review origins"));
                }
            }
        }
        let verified = load_review_receipt(&physical, &v.task_id)?
            .ok_or_else(|| bad("canonical review disappeared"))?;
        return Ok(ReviewOutcome {
            decisions: verified.decision_locators,
            prepared: retained.as_ref().map(|c| c.prepared.clone()),
            status: retained.as_ref().map(|c| c.status),
            disposition: if retained.is_some() {
                ReviewOutcomeDisposition::RetainedChange
            } else {
                ReviewOutcomeDisposition::CanonicalRestored
            },
            allocations: r.allocations.clone(),
            summary: summary(&current)?,
            reused: true,
        });
    }
    engine.plan(&ChangeDraft {
        title: "Recheck complete review inputs".into(),
        origin: None,
        inverse_of: None,
        allocated_ids: BTreeMap::new(),
        read_preconditions: v.dependencies.clone(),
        operations: vec![],
    })?;
    let closed = SourceView::from_closed_input(engine.fs(), &current.input)?;
    let prepared =
        engine.prepare_or_reuse(writer, origin, OriginPolicy::ReuseOrConflict, || {
            build_draft(&closed, &current)
        })?;
    let receipt = retained_receipt(engine, &current, &prepared.change, None)?;
    Ok(ReviewOutcome {
        decisions: receipt.decision_locators,
        prepared: Some(prepared.change.prepared),
        status: Some(prepared.change.status),
        disposition: ReviewOutcomeDisposition::RetainedChange,
        allocations: receipt.receipt.allocations,
        summary: summary(&current)?,
        reused: prepared.reused,
    })
}
#[derive(Debug)]
pub struct VerifiedReviewInverse {
    accepted: BTreeSet<RecordId>,
}
impl VerifiedReviewInverse {
    pub fn restored_accepted(&self) -> &BTreeSet<RecordId> {
        &self.accepted
    }
}
/// Authenticate the original allocated carrier/reference family from retained
/// proposed bytes. This does not replace exact write-set/publication validation
/// by the committed-anchor verifier or the selected sealed projector.
pub(crate) fn retained_review_family(
    anchor: &RetainedGraphInput,
    actual_vault: &RecordId,
) -> Result<(
    ReviewReceiptV1,
    Option<super::normalized_types::CanonicalGraphOriginV2>,
)> {
    if anchor.origin().operation != OriginOperation::GraphReview {
        return Err(bad("review inverse anchor origin differs"));
    }
    let mut notes = BTreeMap::new();
    for op in anchor.operations() {
        if op.before() == &ExpectedState::Absent
            && let Some(b) = op.after_bytes()
        {
            let n = parse_note(b);
            if has_fence(&n) {
                if notes.insert(op.path().clone(), n).is_some() {
                    return Err(bad("review inverse anchor duplicate proof path"));
                }
            }
        }
    }
    let mut collected = collect_review_receipts_v2(&notes, &mut ReceiptBudget::default())?;
    if collected.receipts.len() != 1 {
        return Err(bad(
            "review inverse committed anchor receipt missing/ambiguous",
        ));
    }
    let r = collected
        .receipts
        .remove(&anchor.origin().packet_id)
        .ok_or_else(|| bad("review inverse committed anchor receipt missing"))?;
    let origin = collected.origins.remove(&r.task_id);
    if origin
        .as_ref()
        .is_some_and(|origin| &origin.vault_id != actual_vault)
    {
        return Err(bad("review anchor origin belongs to another vault"));
    }
    if r.task_id != anchor.origin().packet_id
        || r.request_hash != anchor.origin().response_hash
        || allocation_map(&r.allocations) != *anchor.allocated_ids()
    {
        return Err(bad("review inverse anchor allocation/origin differs"));
    }
    Ok((r, origin))
}
// Authenticated committed seal only; no untrusted historical mode is exposed.
pub(crate) fn verify_review_committed_anchor(
    fs: &VaultFs,
    input: &ValidationInput,
    w: &RetainedGraphInverseInput,
) -> Result<ReviewReceiptV1> {
    let (r, origin) = retained_review_family(w.anchor(), &input.vault_id)?;
    let logical = logical_input(input, Some(w.anchor()))?;
    let old = SourceView::from_closed_input(fs, &logical)?;
    let expected = expected_writes_versioned(&old, &r, origin.as_ref())?;
    if expected.len() != w.anchor().operations().len() {
        return Err(bad("review committed anchor exact write set differs"));
    }
    for op in w.anchor().operations() {
        let want = expected
            .get(op.path())
            .ok_or_else(|| bad("review committed anchor extra operation"))?;
        if op.role() != OperationRole::MutableRecord
            || op.before() != &want.expected
            || op.after() != &digest(want.proposed.as_deref())
            || op.after_bytes() != want.proposed.as_deref()
        {
            return Err(bad(
                "review committed anchor exact original operations differ",
            ));
        }
    }
    Ok(r)
}
pub fn verify_review_inverse_overlay(
    fs: &VaultFs,
    input: &ValidationInput,
    w: &RetainedGraphInverseInput,
) -> Result<VerifiedReviewInverse> {
    bounded(input)?;
    if !(1..=MAX_REVIEW_HISTORY_HOPS).contains(&w.inversion_depth())
        || w.inversion_depth() == 1
            && (w.parent_change_id() != w.anchor().change_id()
                || w.parent_manifest_hash() != w.anchor().manifest_hash())
    {
        return Err(bad("review inverse bounded ancestry/parent differs"));
    }
    let r = verify_review_committed_anchor(fs, input, w)?;
    let actual = input
        .documents
        .iter()
        .map(|d| (&d.path, d.bytes.as_slice()))
        .collect::<BTreeMap<_, _>>();
    let overlay = input
        .overlay
        .iter()
        .map(|o| (&o.path, o.bytes.as_deref()))
        .collect::<BTreeMap<_, _>>();
    let ops = w
        .parent_operations()
        .iter()
        .filter(|o| o.role() != OperationRole::ImmutableAsset)
        .collect::<Vec<_>>();
    let paths = ops.iter().map(|o| o.path()).collect::<BTreeSet<_>>();
    if overlay.len() != input.overlay.len()
        || paths.len() != ops.len()
        || overlay.keys().copied().collect::<BTreeSet<_>>() != paths
    {
        return Err(bad("review inverse exact mutable target set differs"));
    }
    let mut accepted = BTreeSet::new();
    let mut deleted = BTreeSet::new();
    for op in ops {
        if digest(op.before_bytes()) != *op.before() || digest(op.after_bytes()) != *op.after() {
            return Err(bad("review inverse parent byte hashes differ"));
        }
        let physical = actual.get(op.path()).copied();
        if digest(physical) != *op.before() && digest(physical) != *op.after() {
            return Err(conflict(
                "review inverse target contains author/unfamiliar edits",
            ));
        }
        if overlay.get(op.path()).copied() != Some(op.before_bytes()) {
            return Err(bad("review inverse is not exact parent before bytes"));
        }
        if let Some(b) = op.before_bytes() {
            let n = parse_note(b);
            if let Some(rec) = &n.canonical
                && rec.kind() == RecordKind::Assertion
                && rec.string("wiki_status") == Some("accepted")
            {
                if !r.allocations.decisions.contains_key(rec.id()) {
                    return Err(bad(
                        "review inverse accepted assertion outside original authority",
                    ));
                }
                accepted.insert(rec.id().clone());
            }
        } else if let Some(b) = op.after_bytes()
            && let Some(rec) = parse_note(b).canonical
        {
            deleted.insert(rec.id().clone());
        }
    }
    let current = SourceView::from_closed_input(
        fs,
        &ValidationInput {
            vault_id: input.vault_id.clone(),
            documents: input.documents.clone(),
            overlay: vec![],
        },
    )?;
    let final_view = SourceView::from_closed_input(fs, input)?;
    for (p, n) in &current.notes {
        if !paths.contains(p) && references_deleted(n, &deleted) {
            return Err(conflict(
                "new canonical reference prevents review inverse deletion",
            ));
        }
    }
    for n in final_view.notes.values() {
        if references_deleted(n, &deleted) {
            return Err(conflict(
                "review inverse leaves reference to deleted evidence/Decision",
            ));
        }
    }
    for p in &r.assertion_proofs {
        if final_view
            .resolve(&p.assertion_id, RecordKind::Assertion, None)
            .is_ok_and(|(_, n)| status(n).is_ok_and(|s| s == "accepted"))
        {
            accepted.insert(p.assertion_id.clone());
        }
    }
    // Current source support is re-established by Catalog for this complete set,
    // including already-restored accepted targets in physical partial recovery.
    Ok(VerifiedReviewInverse { accepted })
}
fn references_deleted(n: &ParsedNote, deleted: &BTreeSet<RecordId>) -> bool {
    let Some(r) = &n.canonical else { return false };
    r.fields().iter().any(|(k, v)| {
        k.starts_with("wiki_")
            && k != "wiki_id"
            && (k.ends_with("_id") || k.ends_with("_ids"))
            && match v {
                Value::String(s) => RecordId::new(s).is_ok_and(|id| deleted.contains(&id)),
                Value::Array(v) => v.iter().any(|s| {
                    s.as_str()
                        .is_some_and(|s| RecordId::new(s).is_ok_and(|id| deleted.contains(&id)))
                }),
                _ => false,
            }
    })
}

#[cfg(test)]
#[path = "receipt_review_budget_tests.rs"]
mod receipt_budget_tests;

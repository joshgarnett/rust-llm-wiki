//! A bounded selected canonical closure, never a complete-vault freshness claim.
use super::{context_types::VerificationBudget, verification::Meter};
use crate::{
    catalog::{
        Catalog, DocumentRow, RecordRow, eligibility,
        eligibility_facts::{EligibilityFact, EligibilityRole},
        eligibility_rules::{self as rules, GenerationContext},
        query::QuerySnapshot,
        query_types::QueryCatalog,
        row_projection,
    },
    changes::{ProposedTarget, ScanDocument, ValidationInput},
    domain::*,
    records::parse_note,
    sources::{
        CitationScope, SourceView,
        evidence::evidence_reference,
        selected::{SelectedSourceBinding, verify_captured_source},
    },
    vault::ExpectedState,
};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) struct SelectedDocuments {
    pub(crate) records: BTreeMap<RecordId, RecordRow>,
    pub(crate) documents: BTreeMap<VaultRelativePath, DocumentRow>,
    pub(crate) fingerprint: Blake3Hash,
    states: BTreeMap<VaultRelativePath, ExpectedState>,
    meter: Meter,
}
fn conflict(message: impl Into<String>) -> WikiError {
    let mut error = WikiError::new(ErrorCode::FreshnessConflict, message);
    error.hint = Some("Inspect the selected files and dependencies. If the external edits are intended, run index sync and retry.".into());
    error
}
impl SelectedDocuments {
    pub(super) fn meter(&self) -> &Meter {
        &self.meter
    }
    pub(crate) fn recheck(&mut self, catalog: &Catalog, reader: &QuerySnapshot) -> Result<()> {
        self.meter.check()?;
        reader.verify_operations(catalog)?;
        for (path, expected) in &self.states {
            let actual = self
                .meter
                .read(catalog, path)?
                .map_or(ExpectedState::Absent, |bytes| {
                    ExpectedState::Hash(Blake3Hash::digest(bytes))
                });
            if &actual != expected {
                return Err(conflict(format!(
                    "selected dependency changed before emission: {path}"
                )));
            }
        }
        reader.verify_operations(catalog)?;
        self.meter.check()
    }
}
fn capture(
    catalog: &Catalog,
    path: &VaultRelativePath,
    expected: &ExpectedState,
    bytes: &mut BTreeMap<VaultRelativePath, Vec<u8>>,
    states: &mut BTreeMap<VaultRelativePath, ExpectedState>,
    meter: &mut Meter,
) -> Result<()> {
    if let Some(old) = states.get(path) {
        if old != expected {
            return Err(conflict(
                "selected dependency has conflicting expected states",
            ));
        }
        return meter.check();
    }
    let read = meter.read(catalog, path)?;
    let actual = read.as_ref().map_or(ExpectedState::Absent, |b| {
        ExpectedState::Hash(Blake3Hash::digest(b))
    });
    if &actual != expected {
        return Err(conflict(format!(
            "selected dependency differs from pinned catalog: {path}"
        )));
    }
    states.insert(path.clone(), expected.clone());
    if let Some(read) = read {
        bytes.insert(path.clone(), read);
    }
    meter.check()
}
fn roles(record: &CanonicalRecord) -> Vec<EligibilityRole> {
    let mut roles = vec![
        EligibilityRole::DeclaredSupport,
        EligibilityRole::ExtractionSource,
        EligibilityRole::ExtractionRevision,
        EligibilityRole::DecisionInput,
        EligibilityRole::DecisionOutput,
        EligibilityRole::AssertionEvidence,
        EligibilityRole::GenerationPacket,
        EligibilityRole::PolicySupersession,
    ];
    roles.extend(
        eligibility::references(record)
            .into_iter()
            .map(|(field, _, _)| EligibilityRole::TypedReference {
                field: field.into(),
            }),
    );
    roles
}
fn payload(row: &RecordRow, field: &str) -> Result<VaultRelativePath> {
    let parent = row
        .path
        .as_str()
        .rsplit_once('/')
        .map(|(p, _)| p)
        .ok_or_else(|| conflict("revision directory absent"))?;
    let name = row
        .record
        .string(field)
        .ok_or_else(|| conflict(format!("revision lacks {field}")))?;
    VaultRelativePath::new(format!("{parent}/{name}"))
}

pub(crate) fn authenticate(
    catalog: &Catalog,
    reader: &QuerySnapshot,
    paths: &[VaultRelativePath],
    budget: &VerificationBudget,
) -> Result<SelectedDocuments> {
    let mut meter = Meter::new(budget);
    meter.check()?;
    reader.require_fact_layout()?;
    reader.verify_operations(catalog)?;
    let mut records = BTreeMap::new();
    let mut documents = BTreeMap::new();
    let mut facts: BTreeMap<RecordId, EligibilityFact> = BTreeMap::new();
    let mut edges = BTreeMap::new();
    let mut bytes = BTreeMap::new();
    let mut states = BTreeMap::new();
    let wiki_path = VaultRelativePath::new("WIKI.md")?;
    let wiki_expected = reader
        .direct_path_states(std::slice::from_ref(&wiki_path))?
        .pop()
        .ok_or_else(|| conflict("vault marker proof missing"))?
        .expected;
    capture(
        catalog,
        &wiki_path,
        &wiki_expected,
        &mut bytes,
        &mut states,
        &mut meter,
    )?;
    let wiki = parse_note(
        bytes
            .get(&wiki_path)
            .ok_or_else(|| conflict("vault marker absent"))?,
    );
    if !wiki
        .canonical
        .as_ref()
        .is_some_and(|r| r.kind() == RecordKind::Vault && r.id() == catalog.vault_id())
    {
        return Err(conflict("vault marker identity changed"));
    }
    let mut pending = vec![catalog.vault_id().clone()];
    let mut required = BTreeSet::from([catalog.vault_id().clone()]);
    let mut allowed_missing = BTreeSet::new();
    let mut missing = BTreeSet::new();
    for path in paths {
        meter.check()?;
        if documents.contains_key(path) {
            continue;
        }
        let document = reader
            .document(path)?
            .ok_or_else(|| conflict(format!("selected document missing: {path}")))?;
        capture(
            catalog,
            path,
            &ExpectedState::Hash(document.hash.clone()),
            &mut bytes,
            &mut states,
            &mut meter,
        )?;
        pending.extend(document.record_id.iter().cloned());
        pending.extend(document.source_id.iter().cloned());
        pending.extend(document.owner_revision.iter().cloned());
        required.extend(document.record_id.iter().cloned());
        required.extend(document.source_id.iter().cloned());
        required.extend(document.owner_revision.iter().cloned());
        documents.insert(path.clone(), document);
    }
    while let Some(id) = pending.pop() {
        meter.check()?;
        if records.contains_key(&id) || missing.contains(&id) {
            continue;
        }
        let Some(row) = reader.record(&id)? else {
            if allowed_missing.contains(&id)
                && !required.contains(&id)
                && reader.unique_identity_claim(&id)?.is_none()
                && reader.eligibility_fact(&id)?.is_none()
            {
                missing.insert(id);
                continue;
            }
            return Err(conflict(format!("selected proof record missing: {id}")));
        };
        let fact = reader
            .eligibility_fact(&id)?
            .ok_or_else(|| conflict("selected eligibility fact missing"))?;
        if fact.structural.baseline() != fact.baseline {
            return Err(conflict(
                "selected structural provenance differs from baseline",
            ));
        }
        let claim = reader
            .unique_identity_claim(&id)?
            .ok_or_else(|| conflict("selected identity claim missing"))?;
        if claim.path != row.path || claim.hash != row.hash || claim.kind != Some(row.record.kind())
        {
            return Err(conflict(
                "selected identity claim differs from cached record",
            ));
        }
        for chunk in fact
            .direct_paths
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .chunks(4096)
        {
            for dep in reader.direct_path_states(chunk)? {
                capture(
                    catalog,
                    &dep.path,
                    &dep.expected,
                    &mut bytes,
                    &mut states,
                    &mut meter,
                )?;
            }
        }
        if row.record.kind() == RecordKind::Revision {
            // Canonical immutable payload declarations are required proof inputs
            // even if a damaged cached direct-path inventory omits them.
            let required: Vec<_> = ["wiki_original_path", "wiki_content_path"]
                .into_iter()
                .filter(|field| row.record.string(field).is_some())
                .map(|field| payload(&row, field))
                .collect::<Result<_>>()?;
            for dep in reader.direct_path_states(&required)? {
                if !fact.direct_paths.contains(&dep.path) {
                    return Err(conflict("revision proof omits declared payload path"));
                }
                capture(
                    catalog,
                    &dep.path,
                    &dep.expected,
                    &mut bytes,
                    &mut states,
                    &mut meter,
                )?;
            }
        }
        let raw = bytes
            .get(&row.path)
            .ok_or_else(|| conflict("selected canonical record absent"))?;
        if matches!(
            row.record.kind(),
            RecordKind::Source | RecordKind::Revision | RecordKind::Vault
        ) && raw.len() > 1024 * 1024
        {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "selected control note exceeds byte ceiling",
            ));
        }
        if parse_note(raw).canonical.as_ref() != Some(&row.record)
            || Blake3Hash::digest(raw) != row.hash
            || row.authored_status.as_deref() != row.record.string("wiki_status")
            || row.record.id() != &id
        {
            return Err(conflict(
                "selected cached record differs from canonical bytes",
            ));
        }
        if row.record.kind() == RecordKind::RunEvent
            && row.record.string("wiki_event_type") == Some("generation_output")
        {
            return Err(WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "selected generation output proof is unavailable; use explicit cached read",
            ));
        }
        // Policy membership is selected by indexed reverse incidence, including
        // valid decisions that produced no retained structural error effect.
        pending.extend(
            reader
                .dependent_edges(
                    &id,
                    &[
                        EligibilityRole::DecisionInput,
                        EligibilityRole::DecisionOutput,
                        EligibilityRole::PolicySupersession,
                        EligibilityRole::TypedReference {
                            field: "wiki_supersedes_id".into(),
                        },
                        EligibilityRole::TypedReference {
                            field: "wiki_superseded_by_id".into(),
                        },
                    ],
                )?
                .into_iter()
                .map(|e| e.owner_id),
        );
        let outgoing = reader.outgoing_edges(&id, &roles(&row.record))?;
        if row.record.kind() == RecordKind::Assertion {
            let support: BTreeSet<_> = outgoing
                .iter()
                .filter(|e| e.role == EligibilityRole::AssertionEvidence)
                .map(|e| e.target_id.clone())
                .collect();
            let owners: BTreeSet<_> = reader
                .dependent_edges(
                    &id,
                    &[EligibilityRole::TypedReference {
                        field: "wiki_assertion_id".into(),
                    }],
                )?
                .into_iter()
                .map(|e| e.owner_id)
                .collect();
            if support != owners {
                return Err(conflict(
                    "selected assertion support membership differs from evidence owners",
                ));
            }
        }
        if row.eligibility == Eligibility::Invalid {
            allowed_missing.extend(outgoing.iter().map(|e| e.target_id.clone()));
        } else {
            required.extend(outgoing.iter().map(|e| e.target_id.clone()));
        }
        pending.extend(outgoing.iter().map(|e| e.target_id.clone()));
        // Nonlocal policy outcomes carry an explicit producer. Authenticate its
        // own proof instead of trusting retained decision effects in isolation.
        pending.extend(
            fact.structural
                .effects
                .iter()
                .filter_map(|e| e.producer.clone()),
        );
        if let Some((key, _)) = rules::opposition_key(&row.record) {
            pending.extend(
                reader
                    .opposition_members(&key)?
                    .into_iter()
                    .map(|(id, _)| id),
            );
        }
        for (field, _, _) in eligibility::references(&row.record) {
            if let Some(target) = row.record.string(field) {
                let target = RecordId::new(target)?;
                if !outgoing.iter().any(|e| {
                    e.role
                        == EligibilityRole::TypedReference {
                            field: field.into(),
                        }
                        && e.target_id == target
                }) {
                    return Err(conflict("selected typed dependency edge missing"));
                }
            }
        }
        let declared: BTreeSet<_> = crate::catalog::scan::list(&row.record, "wiki_depends_on_ids")
            .into_iter()
            .map(RecordId::new)
            .collect::<Result<_>>()?;
        let retained: BTreeSet<_> = outgoing
            .iter()
            .filter(|e| e.role == EligibilityRole::DeclaredSupport)
            .map(|e| e.target_id.clone())
            .collect();
        if declared != retained {
            return Err(conflict("selected declared support edges incomplete"));
        }
        edges.insert(id.clone(), outgoing);
        facts.insert(id.clone(), fact);
        records.insert(id, row);
    }
    if missing.iter().any(|id| required.contains(id)) {
        return Err(conflict("required selected dependency record missing"));
    }
    // Replay dynamic rules against the authenticated forward boundary; never run
    // the complete graph validator over a selected subset.
    let mut actual = records.clone();
    for (id, row) in &mut actual {
        rules::restore_baseline(row, &facts[id].baseline);
    }
    let boundary = actual.clone();
    for row in actual.values_mut() {
        rules::lifecycle(row, &boundary, GenerationContext::NotGeneration)?;
    }
    let record_paths: BTreeSet<_> = records.values().map(|r| r.path.clone()).collect();
    let input = ValidationInput {
        vault_id: catalog.vault_id().clone(),
        documents: records
            .values()
            .map(|r| ScanDocument {
                path: r.path.clone(),
                bytes: bytes[&r.path].clone(),
                hash: r.hash.clone(),
            })
            .collect(),
        overlay: states
            .iter()
            .filter(|(p, _)| !record_paths.contains(*p))
            .map(|(p, _)| ProposedTarget {
                path: p.clone(),
                bytes: bytes.get(p).cloned(),
            })
            .collect(),
    };
    let view = SourceView::from_closed_input(catalog.fs(), &input)?;
    for row in actual.values_mut().filter(|r| {
        r.record.kind() == RecordKind::Evidence && r.eligibility != Eligibility::Invalid
    }) {
        let reference = evidence_reference(&row.record)?;
        let verified = view.verify(
            &CitationRef::Assertion(reference.clone()),
            CitationScope::Historical,
        )?;
        for dep in verified.dependencies {
            if states.get(&dep.path) != Some(&dep.expected) {
                return Err(conflict(
                    "evidence verification escaped selected dependency closure",
                ));
            }
        }
        let source = records
            .get(&reference.source_id)
            .ok_or_else(|| conflict("evidence source boundary missing"))?;
        rules::evidence_lifecycle(row, &source.record)?;
    }
    let boundary = actual.clone();
    for (id, row) in actual
        .iter_mut()
        .filter(|(_, r)| r.record.kind() == RecordKind::Assertion)
    {
        let expected: BTreeSet<_> = edges[id]
            .iter()
            .filter(|e| e.role == EligibilityRole::AssertionEvidence)
            .map(|e| e.target_id.clone())
            .collect();
        let associated: Vec<_> = expected
            .iter()
            .map(|id| {
                boundary
                    .get(id)
                    .ok_or_else(|| conflict("assertion evidence boundary missing"))
            })
            .collect::<Result<_>>()?;
        rules::assertion_support(row, &expected, &associated)?;
    }
    for _ in 0..actual.len() {
        meter.check()?;
        let boundary = actual.clone();
        let mut changed = false;
        for row in actual.values_mut() {
            let targets = crate::catalog::scan::list(&row.record, "wiki_depends_on_ids")
                .into_iter()
                .map(|id| {
                    let id = RecordId::new(id)?;
                    let eligibility = boundary.get(&id).map(|r| r.eligibility);
                    if eligibility.is_none() && !missing.contains(&id) {
                        return Err(conflict("declared dependency missing"));
                    }
                    Ok((id, eligibility))
                })
                .collect::<Result<BTreeMap<_, _>>>()?;
            changed |= rules::declared_dependencies(row, &targets)?;
        }
        if !changed {
            break;
        }
    }
    let mut opposition: BTreeMap<_, [Vec<RecordId>; 2]> = BTreeMap::new();
    for row in actual.values().filter(|r| rules::eligible_opposition(r)) {
        if let Some((key, negated)) = rules::opposition_key(&row.record) {
            opposition.entry(key).or_default()[usize::from(negated)].push(row.record.id().clone());
        }
    }
    for sides in opposition
        .values()
        .filter(|s| !s[0].is_empty() && !s[1].is_empty())
    {
        for id in sides.iter().flatten() {
            rules::mark_opposition(actual.get_mut(id).expect("bounded opposition member"));
        }
    }
    for (id, row) in &mut actual {
        rules::finalize(row);
        if row != &records[id] {
            return Err(conflict(format!(
                "selected cached eligibility differs from authenticated rules: {id}"
            )));
        }
    }
    for document in documents.values() {
        let expected = if let (Some(source_id), Some(revision_id)) =
            (&document.source_id, &document.owner_revision)
        {
            let source = &records[source_id];
            let revision = &records[revision_id];
            let original_path = payload(revision, "wiki_original_path")?;
            let content_path = payload(revision, "wiki_content_path")?;
            let binding = SelectedSourceBinding {
                vault_id: catalog.vault_id().clone(),
                source_id: source_id.clone(),
                revision_id: revision_id.clone(),
                source_path: source.path.clone(),
                revision_path: revision.path.clone(),
                content_path: document.path.clone(),
                source_hash: source.hash.clone(),
                revision_hash: revision.hash.clone(),
                content_hash: document.hash.clone(),
            };
            let mut total = 0usize;
            let captured = [
                &wiki_path,
                &source.path,
                &revision.path,
                &original_path,
                &content_path,
            ]
            .into_iter()
            .map(|p| {
                let b = bytes
                    .get(p)
                    .ok_or_else(|| conflict("source payload omitted from selected proof"))?;
                total = total
                    .checked_add(b.len())
                    .ok_or_else(|| conflict("source capture overflow"))?;
                if b.len() > 64 * 1024 * 1024 || total > 128 * 1024 * 1024 {
                    return Err(WikiError::new(
                        ErrorCode::BudgetExceeded,
                        "source capture allocation ceiling exceeded",
                    ));
                }
                Ok((p.clone(), b.clone()))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
            let (content, dependencies) = if revision.eligibility == Eligibility::Current {
                let proof = verify_captured_source(catalog.fs(), &binding, &captured)?;
                (
                    proof.content,
                    proof
                        .dependencies
                        .into_iter()
                        .map(|d| (d.path, d.expected))
                        .collect::<BTreeMap<_, _>>(),
                )
            } else {
                let mut dependencies = BTreeMap::new();
                let content = view.revision_content_bounded(
                    source_id,
                    revision_id,
                    &mut dependencies,
                    64 * 1024 * 1024,
                    64 * 1024 * 1024,
                )?;
                if content_path != document.path {
                    return Err(conflict("historical content owner path differs"));
                }
                (content, dependencies)
            };
            for (path, expected) in dependencies {
                if states.get(&path) != Some(&expected) {
                    return Err(conflict(
                        "source verifier escaped selected dependency closure",
                    ));
                }
            }
            row_projection::captured_content_document(
                document.path.clone(),
                source_id.clone(),
                revision,
                String::from_utf8(content).map_err(|_| conflict("source content is not UTF-8"))?,
            )
        } else {
            if document.source_id.is_some() || document.owner_revision.is_some() {
                return Err(conflict("selected capture owner binding incomplete"));
            }
            let note = parse_note(&bytes[&document.path]);
            let row = document.record_id.as_ref().map(|id| &records[id]);
            if row.is_some_and(|r| r.path != document.path) {
                return Err(conflict("selected document record path differs"));
            }
            row_projection::canonical_document(&document.path, &note, row)
        };
        if &expected != document {
            return Err(conflict(
                "selected cached document differs from canonical rendering",
            ));
        }
    }
    meter.check()?;
    let encoded = serde_json::to_vec(&(
        "lwiki-indexed-documents-selected-dependencies-v1",
        catalog.vault_id(),
        reader.publication_id(),
        reader.snapshot(),
        &states,
        &missing,
    ))
    .map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))?;
    let fingerprint = Blake3Hash::digest(encoded);
    Ok(SelectedDocuments {
        records,
        documents,
        fingerprint,
        states,
        meter,
    })
}

#[cfg(test)]
#[path = "selected_documents_tests.rs"]
mod tests;

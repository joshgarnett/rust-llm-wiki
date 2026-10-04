//! Sealed admission for bounded canonical writes and one atomic publication.
use super::{
    eligibility_facts::EligibilityFact,
    link_facts,
    normalized_delta::{CatalogDelta, OwnedDiagnostics},
    normalized_fact_delta::{FactDelta, OwnedRegistryKeys, RecordFactMutation},
    policy_delta::PolicyDelta,
    policy_projection::{self, PolicyInputAccess},
    query::QuerySnapshot,
    query_types::QueryCatalog,
    source_projection::{
        self, RefreshProjectionLimits, Work, baseline, budget, conflict, deps, entry,
    },
    structural_projection,
    types::RecordRow,
};
use crate::{
    changes::{
        ChangeDraft, ReadDependency,
        indexed_refresh::{IndexedPageTarget, IndexedWriteOperation},
    },
    domain::{
        Blake3Hash, Eligibility, ReadSnapshot, RecordKind, Result, VaultRelativePath, WikiError,
    },
    graph::policy_inputs::PolicyWork,
    records::{ParsedNote, parse_note},
    sources::revision::canonical_path,
    vault::{ExpectedState, VaultFs},
};
use std::collections::{BTreeMap, BTreeSet};

/// Constructed only after the complete selected overlay has been validated.
pub(crate) struct ProjectedWrite {
    parts: ProjectedWriteParts,
}
pub(super) struct ProjectedWriteParts {
    pub operation: IndexedWriteOperation,
    pub draft: ChangeDraft,
    pub base: ReadSnapshot,
    pub before: Vec<ReadDependency>,
    pub after: Vec<ReadDependency>,
    pub delta: CatalogDelta,
}
impl ProjectedWrite {
    pub(super) fn from_parts(parts: ProjectedWriteParts) -> Self {
        Self { parts }
    }
    pub(crate) fn draft(&self) -> &ChangeDraft {
        &self.parts.draft
    }
    pub(super) fn into_parts(self) -> ProjectedWriteParts {
        self.parts
    }
}
impl PolicyInputAccess for Work<'_> {
    fn note(&mut self, path: &VaultRelativePath, expected: &Blake3Hash) -> Result<ParsedNote> {
        self.capture(path, &ExpectedState::Hash(expected.clone()))?;
        Work::note(self, path)
    }
    fn work(&mut self, value: PolicyWork) -> Result<()> {
        // Every retry shares the enclosing admission's deadline, visits and bytes.
        self.visits = self.visits.checked_add(value.steps).ok_or_else(budget)?;
        self.charge(value.bytes)
    }
}

pub(super) fn project_policy(work: &mut Work<'_>) -> Result<PolicyDelta> {
    let mut before = BTreeMap::new();
    let mut overlay = BTreeMap::new();
    for (path, bytes) in &work.overlay {
        if canonical_path(path) {
            if let Some(old) = work.captured.get(path) {
                before.insert(path.clone(), parse_note(&old.bytes));
            }
            overlay.insert(path.clone(), parse_note(bytes));
        }
    }
    policy_projection::project_policy(work.reader, &before, &overlay, work)
}

pub(super) fn empty_delta(policy: PolicyDelta) -> CatalogDelta {
    CatalogDelta {
        version: 3,
        records: vec![],
        documents: vec![],
        graph: vec![],
        links: vec![],
        diagnostics: vec![],
        claims: vec![],
        revisions: vec![],
        dependencies: vec![],
        owners: vec![],
        facts: Some(FactDelta {
            policy: Some(policy),
            records: vec![],
            edge_inserts: vec![],
            edge_deletes: vec![],
            links: vec![],
            registry: vec![],
        }),
    }
}

/// The draft is merely input: every operation, identity and before-image is
/// checked here before it can obtain normalized publication authority.
pub(crate) fn project_pages(
    fs: &VaultFs,
    reader: &QuerySnapshot,
    mut draft: ChangeDraft,
    limits: &RefreshProjectionLimits,
) -> Result<Option<ProjectedWrite>> {
    reader.require_policy_layout()?;
    let base = QueryCatalog::snapshot(reader).clone();
    if base.publication().is_none() {
        return Err(conflict("Page admission requires a pinned publication"));
    }
    if draft.origin.is_some()
        || draft.inverse_of.is_some()
        || !draft.allocated_ids.is_empty()
        || draft.title.trim().is_empty()
        || draft.title.len() > 4096
    {
        return Err(WikiError::invalid(
            "Page admission requires an ordinary titled Page draft",
        ));
    }
    if draft.operations.is_empty()
        || draft.operations.len() > 16
        || draft.read_preconditions.len() > 128
    {
        return Err(budget());
    }
    let total = draft
        .operations
        .iter()
        .try_fold(0usize, |n, op| {
            n.checked_add(op.proposed.as_ref().map_or(0, Vec::len))
        })
        .ok_or_else(budget)?;
    if total > 16 * 1024 * 1024 {
        return Err(budget());
    }
    let mut work = Work::new(fs, reader, limits)?;
    work.capture_path(&VaultRelativePath::new("WIKI.md")?)?;
    for dependency in &draft.read_preconditions {
        work.capture(&dependency.path, &dependency.expected)?;
    }
    let mut paths = BTreeSet::new();
    let mut identities = BTreeSet::new();
    let mut pages = Vec::new();
    let mut changed = Vec::new();
    for operation in &draft.operations {
        work.tick()?;
        if !canonical_path(&operation.target)
            || !operation.apply_after.is_empty()
            || !paths.insert(operation.target.clone())
        {
            return Err(WikiError::invalid(
                "Page batch has a reserved, duplicate or ordered target",
            ));
        }
        let bytes = operation
            .proposed
            .as_ref()
            .ok_or_else(|| WikiError::invalid("Page admission cannot delete files"))?;
        work.charge(bytes.len())?;
        let note = parse_note(bytes);
        let record = note
            .canonical
            .as_ref()
            .filter(|r| r.kind() == RecordKind::Page)
            .ok_or_else(|| WikiError::invalid("Page operation lacks a valid Page envelope"))?;
        let id = record.id().clone();
        if !identities.insert(id.clone()) {
            return Err(WikiError::invalid("Page batch has duplicate identities"));
        }
        work.capture(&operation.target, &operation.expected)?;
        let claim = reader.unique_identity_claim(&id)?;
        let existing = work.load(&id)?;
        match &operation.expected {
            ExpectedState::Absent => {
                if claim.is_some()
                    || existing
                    || reader.document_metadata(&operation.target)?.is_some()
                {
                    return Err(conflict(
                        "new Page identity or path is already claimed by the publication",
                    ));
                }
            }
            ExpectedState::Hash(hash) => {
                let old = work
                    .old
                    .get(&id)
                    .ok_or_else(|| conflict("Page replacement lacks an adopted prior identity"))?;
                if old.path != operation.target
                    || old.hash != *hash
                    || old.record.kind() != RecordKind::Page
                    || claim.as_ref().is_none_or(|c| {
                        c.path != old.path || c.hash != old.hash || c.kind != Some(RecordKind::Page)
                    })
                {
                    return Err(conflict(
                        "Page replacement must preserve authenticated identity, kind and path",
                    ));
                }
            }
        }
        if work
            .captured
            .get(&operation.target)
            .is_some_and(|old| old.bytes == *bytes)
        {
            continue;
        }
        work.overlay.insert(operation.target.clone(), bytes.clone());
        work.replaced_registry.insert(operation.target.clone());
        let row = RecordRow {
            record: record.clone(),
            path: operation.target.clone(),
            hash: note.source_hash,
            authored_status: record.string("wiki_status").map(str::to_owned),
            eligibility: Eligibility::Current,
            reasons: vec![],
            identity_eligibility: None,
            description_eligibility: None,
            disputed: false,
            dependencies: vec![],
        };
        work.new_registry.push(entry(&row));
        work.now.insert(id.clone(), row);
        work.facts
            .entry(id.clone())
            .or_insert_with(|| EligibilityFact {
                baseline: baseline(),
                structural: Default::default(),
                direct_paths: BTreeSet::from([operation.target.clone()]),
            });
        pages.push(IndexedPageTarget {
            path: operation.target.clone(),
            id,
        });
        changed.push(operation.clone());
    }
    if changed.is_empty() {
        work.recheck()?;
        return Ok(None);
    }
    draft.operations = changed;
    pages.sort_by(|a, b| a.path.cmp(&b.path));
    let mut seeds: BTreeSet<_> = pages.iter().map(|page| page.id.clone()).collect();
    let mut changed_keys = BTreeSet::new();
    for page in &pages {
        if let Some(old) = work.old.get(&page.id) {
            changed_keys.extend(link_facts::registry_keys(&entry(old))?);
        }
        changed_keys.extend(link_facts::registry_keys(&entry(&work.now[&page.id]))?);
    }
    let changed_keys: Vec<_> = changed_keys.into_iter().collect();
    let affected_links = reader.affected_links(&changed_keys)?;
    for (path, offset) in &affected_links {
        if reader
            .link_fact(path, *offset)?
            .is_some_and(|fact| fact.typed.is_some())
        {
            let owner = reader
                .document_metadata(path)?
                .and_then(|row| row.record_id)
                .ok_or_else(|| {
                    source_projection::corrupt("typed navigation owner is not an adopted record")
                })?;
            work.require(&owner)?;
            seeds.insert(owner);
        }
    }
    let policy = project_policy(&mut work)?;
    let mut delta = empty_delta(policy.clone());
    let structural = structural_projection::recompute(&mut work, seeds, &policy, &mut delta)?;
    work.discover(structural.clone())?;
    work.recompute()?;
    for id in &structural {
        let old: BTreeSet<_> = reader
            .outgoing_edges(
                id,
                &[super::eligibility_facts::EligibilityRole::GenerationPacket],
            )?
            .into_iter()
            .collect();
        let new: BTreeSet<_> = work.edge_overrides[id]
            .iter()
            .filter(|edge| edge.role == super::eligibility_facts::EligibilityRole::GenerationPacket)
            .cloned()
            .collect();
        delta
            .facts
            .as_mut()
            .unwrap()
            .edge_deletes
            .extend(old.difference(&new).cloned());
        delta
            .facts
            .as_mut()
            .unwrap()
            .edge_inserts
            .extend(new.difference(&old).cloned());
    }

    for id in &work.dynamic {
        let row = &work.now[id];
        if row.eligibility == Eligibility::Invalid
            && (work.overlay.contains_key(&row.path)
                || work.old.get(id).is_none_or(|old| {
                    old.eligibility != Eligibility::Invalid || old.reasons != row.reasons
                }))
        {
            return Err(WikiError::invalid(format!(
                "proposal invalidates typed record {id}: {}",
                row.reasons.join(",")
            )));
        }
    }
    // The complete structural check above is the Page admission rule: reject
    // newly invalid records and changed invalid-reason sets. Legacy admission
    // allows an existing invalid reference to change missing -> wrong-kind
    // when a named Page appears. Only certified owners bypass refresh's stricter
    // navigation-only comparison; other owners retain that guard.
    work.admitted_typed_navigation
        .extend(structural.iter().map(|id| work.now[id].path.clone()));
    for id in work.dynamic.clone() {
        work.emit_record(&id, &mut delta)?;
        let row = work.now[&id].clone();
        if row.record.kind() == RecordKind::Revision
            && row.record.string("wiki_extraction_status") == Some("complete")
        {
            let content = source_projection::asset_path(
                &row,
                row.record
                    .string("wiki_content_path")
                    .expect("complete revision"),
            )?;
            if reader.document_metadata(&content)?.is_some() {
                work.metadata_document(
                    &content,
                    &row,
                    None,
                    Some((
                        source_projection::record_id(&row.record, "wiki_source_id")?,
                        id.clone(),
                    )),
                    &mut delta,
                )?;
            }
        }
    }
    for id in structural {
        let row = &work.now[&id];
        let fact = &work.facts[&id];
        delta
            .facts
            .as_mut()
            .unwrap()
            .records
            .push(RecordFactMutation {
                record_id: id.clone(),
                fact: fact.clone(),
            });
        let mut diagnostics = reader.diagnostics(&BTreeSet::from([row.path.clone()]))?;
        if let Some(old) = work.old.get(&id) {
            let old_fact = reader
                .eligibility_fact(&id)?
                .ok_or_else(|| source_projection::corrupt("old structural fact missing"))?;
            let prior = old_fact.structural.diagnostics(old);
            diagnostics.retain(|d| !prior.contains(d));
        }
        diagnostics.extend(fact.structural.diagnostics(row));
        diagnostics.retain(|d| {
            d.details.get("reason").and_then(serde_json::Value::as_str)
                != Some("generation_output_source_unresolved")
        });
        if let Some(Some(diagnostic)) = work.lifecycle_diagnostics.get(&id) {
            diagnostics.push(diagnostic.clone());
        }

        if let Some(owned) = delta
            .diagnostics
            .iter_mut()
            .find(|owned| owned.path == row.path)
        {
            owned.rows = diagnostics;
        } else {
            delta.diagnostics.push(OwnedDiagnostics {
                path: row.path.clone(),
                rows: diagnostics,
            });
        }
    }
    let mut keys = BTreeSet::new();
    for page in &pages {
        if let Some(old) = work.old.get(&page.id) {
            keys.extend(link_facts::registry_keys(&entry(old))?);
        }
        let new_keys = link_facts::registry_keys(&entry(&work.now[&page.id]))?;
        keys.extend(new_keys.clone());
        delta
            .facts
            .as_mut()
            .unwrap()
            .registry
            .push(OwnedRegistryKeys {
                record_id: page.id.clone(),
                path: page.path.clone(),
                keys: new_keys,
            });
    }
    let keys: Vec<_> = keys.into_iter().collect();
    let mut links: BTreeSet<_> = pages.iter().map(|page| page.path.clone()).collect();
    links.extend(affected_links.into_iter().map(|(path, _)| path));
    for path in links {
        work.emit_links(&path, &mut delta)?;
    }
    let mut navigation: BTreeSet<_> = reader
        .affected_assertion_navigation(&keys)?
        .into_iter()
        .collect();
    navigation.extend(
        work.dynamic
            .iter()
            .filter(|id| work.now[*id].record.kind() == RecordKind::Assertion)
            .cloned(),
    );
    for id in navigation {
        work.emit_assertion_navigation(&id, &mut delta)?;
    }
    let mut after = work.before.clone();
    for (path, bytes) in &work.overlay {
        after.insert(path.clone(), ExpectedState::Hash(Blake3Hash::digest(bytes)));
    }
    draft.read_preconditions = deps(work.before.clone());
    delta.dependencies = deps(after.clone());
    delta.validate()?;
    work.recheck()?;
    Ok(Some(ProjectedWrite {
        parts: ProjectedWriteParts {
            operation: IndexedWriteOperation::PageBatch { pages },
            draft,
            base,
            before: deps(work.before),
            after: deps(after),
            delta,
        },
    }))
}

#[cfg(test)]
#[path = "write_projection_tests.rs"]
mod tests;

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

pub(super) fn project_source_refresh_batch_policy(work: &mut Work<'_>) -> Result<PolicyDelta> {
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
    policy_projection::project_policy_for_refresh_batch(work.reader, &before, &overlay, work)
}

pub(super) fn project_policy_move(work: &mut Work<'_>) -> Result<PolicyDelta> {
    let mut before = BTreeMap::new();
    let mut overlay = BTreeMap::new();
    for path in work.overlay.keys().chain(work.removed_paths.iter()) {
        if let Some(old) = work.captured.get(path) {
            before.insert(path.clone(), parse_note(&old.bytes));
        }
    }
    for (path, bytes) in &work.overlay {
        overlay.insert(path.clone(), parse_note(bytes));
    }
    let removed = work.removed_paths.clone();
    policy_projection::project_policy_for_move(work.reader, &before, &overlay, &removed, work)
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
    draft: ChangeDraft,
    limits: &RefreshProjectionLimits,
) -> Result<Option<ProjectedWrite>> {
    project_selected(fs, reader, draft, None, limits)
}

/// The sealed draft was regenerated against the one existing operational ledger.
pub(crate) fn project_jobs(
    fs: &VaultFs,
    reader: &QuerySnapshot,
    validated: crate::jobs::checkpoint::ValidatedJobDraft,
    limits: &RefreshProjectionLimits,
) -> Result<Option<ProjectedWrite>> {
    let (draft, operation) = validated.into_parts();
    project_selected(fs, reader, draft, Some(operation), limits)
}

fn project_selected(
    fs: &VaultFs,
    reader: &QuerySnapshot,
    mut draft: ChangeDraft,
    job: Option<IndexedWriteOperation>,
    limits: &RefreshProjectionLimits,
) -> Result<Option<ProjectedWrite>> {
    reader.require_policy_layout()?;
    if QueryCatalog::snapshot(reader).publication().is_none() {
        return Err(conflict("Page admission requires a pinned publication"));
    }
    if draft.origin.is_some()
        || draft.inverse_of.is_some()
        || (job.is_none() && !draft.allocated_ids.is_empty())
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
        if let Some(IndexedWriteOperation::JobBatch {
            checkpoint: Some(asset),
            ..
        }) = &job
        {
            if operation.target == asset.path {
                let bytes = operation
                    .proposed
                    .as_ref()
                    .ok_or_else(|| WikiError::invalid("compact asset cannot be deleted"))?;
                if operation.expected != ExpectedState::Absent
                    || Blake3Hash::digest(bytes) != asset.hash
                    || !operation.apply_after.is_empty()
                    || !paths.insert(operation.target.clone())
                {
                    return Err(WikiError::invalid(
                        "compact asset differs from sealed job draft",
                    ));
                }
                work.charge(bytes.len())?;
                work.capture(&operation.target, &operation.expected)?;
                work.overlay.insert(operation.target.clone(), bytes.clone());
                changed.push(operation.clone());
                continue;
            }
        }
        if !canonical_path(&operation.target)
            || (job.is_none() && !operation.apply_after.is_empty())
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
            .filter(|r| {
                if let Some(IndexedWriteOperation::JobBatch { records, .. }) = &job {
                    records.iter().any(|target| {
                        target.path == operation.target
                            && target.id == *r.id()
                            && target.kind == r.kind()
                    })
                } else {
                    r.kind() == RecordKind::Page
                }
            })
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
                    || old.record.kind() != record.kind()
                    || claim.as_ref().is_none_or(|c| {
                        c.path != old.path || c.hash != old.hash || c.kind != Some(record.kind())
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
    finish_pages(work, draft, pages, job, false)
}

fn finish_pages(
    mut work: Work<'_>,
    draft: ChangeDraft,
    mut pages: Vec<IndexedPageTarget>,
    job: Option<IndexedWriteOperation>,
    external: bool,
) -> Result<Option<ProjectedWrite>> {
    let mut draft = draft;
    let base = QueryCatalog::snapshot(work.reader).clone();
    let reader = work.reader;
    pages.sort_by(|a, b| a.path.cmp(&b.path));
    let mut seeds: BTreeSet<_> = pages.iter().map(|page| page.id.clone()).collect();
    let mut changed_keys = BTreeSet::new();
    for page in &pages {
        if let Some(old) = work.old.get(&page.id) {
            changed_keys.extend(link_facts::registry_keys(&entry(old))?);
        }
        if let Some(now) = work.now.get(&page.id) {
            changed_keys.extend(link_facts::registry_keys(&entry(now))?);
        }
    }
    let changed_keys: Vec<_> = changed_keys.into_iter().collect();
    let affected_links = reader.affected_links(&changed_keys)?;
    for (path, offset) in &affected_links {
        if work.removed_paths.contains(path) {
            continue;
        }
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
    let policy = if external {
        let before = pages
            .iter()
            .filter_map(|page| {
                work.captured
                    .get(&page.path)
                    .map(|old| (page.path.clone(), parse_note(&old.bytes)))
            })
            .collect();
        let overlay = work
            .overlay
            .iter()
            .map(|(path, bytes)| (path.clone(), parse_note(bytes)))
            .collect();
        let removed = work.removed_paths.clone();
        policy_projection::project_policy_for_external_pages(
            reader, &before, &overlay, &removed, &mut work,
        )?
    } else {
        project_policy(&mut work)?
    };
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
        if !external
            && row.eligibility == Eligibility::Invalid
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
        let Some(now) = work.now.get(&page.id) else {
            continue;
        };
        let new_keys = link_facts::registry_keys(&entry(now))?;
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
        if work.removed_paths.contains(&path) {
            delta.links.push(super::normalized_delta::OwnedLinks {
                path: path.clone(),
                rows: vec![],
            });
            delta.claims.push(super::normalized_delta::OwnedClaims {
                path: path.clone(),
                rows: vec![],
            });
            delta.diagnostics.push(OwnedDiagnostics {
                path: path.clone(),
                rows: vec![],
            });
            delta.facts.as_mut().unwrap().links.push(
                super::normalized_fact_delta::OwnedLinkFacts {
                    path: path.clone(),
                    rows: vec![],
                },
            );
            delta
                .documents
                .push(super::normalized_delta::DocumentMutation::DeletePage {
                    path: path.clone(),
                    expected_hash: work
                        .old
                        .values()
                        .find(|row| row.path == path)
                        .ok_or_else(|| source_projection::corrupt("retired owner missing"))?
                        .hash
                        .clone(),
                });
        } else {
            work.emit_links(&path, &mut delta)?;
        }
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
    for path in &work.removed_paths {
        after.insert(path.clone(), ExpectedState::Absent);
    }
    draft.read_preconditions = deps(work.before.clone());
    delta.dependencies = deps(after.clone());
    delta.validate()?;
    work.tick()?;
    if !external {
        work.recheck()?;
    }
    Ok(Some(ProjectedWrite {
        parts: ProjectedWriteParts {
            operation: match job {
                Some(IndexedWriteOperation::JobBatch {
                    run_id,
                    records,
                    checkpoint,
                }) => IndexedWriteOperation::JobBatch {
                    run_id,
                    records: records
                        .into_iter()
                        .filter(|r| pages.iter().any(|p| p.path == r.path))
                        .collect(),
                    checkpoint,
                },
                Some(_) => unreachable!("sealed job operation"),
                None => IndexedWriteOperation::PageBatch { pages },
            },
            draft,
            base,
            before: deps(work.before),
            after: deps(after),
            delta,
        },
    }))
}

/// Cache-only projection of a fully classified external Page change set.
/// This grants no canonical write authority and is applied only to a new sibling.
pub(super) fn project_external_pages(
    fs: &VaultFs,
    reader: &QuerySnapshot,
    changes: &[(
        VaultRelativePath,
        Option<super::DocumentRow>,
        Option<ParsedNote>,
    )],
    limits: &RefreshProjectionLimits,
) -> Result<CatalogDelta> {
    let mut work = Work::new(fs, reader, limits)?;
    let mut pages = Vec::new();
    for (path, old, new) in changes {
        let record = new.as_ref().and_then(|n| n.canonical.as_ref());
        let id = if let Some(record) = record {
            record.id().clone()
        } else {
            old.as_ref()
                .and_then(|d| d.record_id.clone())
                .ok_or_else(|| source_projection::corrupt("retired Page has no identity"))?
        };
        if let Some(old) = old {
            work.charge(old.raw_text.len())?;
            work.captured.insert(
                path.clone(),
                crate::changes::ScanDocument {
                    path: path.clone(),
                    hash: old.hash.clone(),
                    bytes: old.raw_text.as_bytes().to_vec(),
                },
            );
            work.observe(path, ExpectedState::Hash(old.hash.clone()))?;
            if !work.load(&id)? {
                return Err(source_projection::corrupt("cached Page row absent"));
            }
        } else {
            work.observe(path, ExpectedState::Absent)?;
        }
        work.replaced_registry.insert(path.clone());
        if let Some(note) = new {
            work.charge(note.raw.len())?;
            let record = note
                .canonical
                .as_ref()
                .ok_or_else(|| source_projection::corrupt("new Page envelope absent"))?;
            let row = RecordRow {
                record: record.clone(),
                path: path.clone(),
                hash: note.source_hash.clone(),
                authored_status: record.string("wiki_status").map(str::to_owned),
                eligibility: Eligibility::Current,
                reasons: vec![],
                identity_eligibility: None,
                description_eligibility: None,
                disputed: false,
                dependencies: vec![],
            };
            work.overlay.insert(path.clone(), note.raw.clone());
            work.new_registry.push(entry(&row));
            work.now.insert(id.clone(), row);
            work.facts
                .entry(id.clone())
                .or_insert_with(|| EligibilityFact {
                    baseline: baseline(),
                    structural: Default::default(),
                    direct_paths: BTreeSet::from([path.clone()]),
                });
        } else {
            work.removed_paths.insert(path.clone());
            work.now.remove(&id);
            work.edge_overrides.insert(id.clone(), BTreeSet::new());
        }
        pages.push(IndexedPageTarget {
            path: path.clone(),
            id,
        });
    }
    let draft = ChangeDraft {
        title: "External authored Page reconciliation".into(),
        origin: None,
        inverse_of: None,
        allocated_ids: BTreeMap::new(),
        read_preconditions: vec![],
        operations: vec![],
    };
    finish_pages(work, draft, pages, None, true)?
        .map(|projected| projected.into_parts().delta)
        .ok_or_else(|| source_projection::corrupt("external Page projection unexpectedly empty"))
}

#[cfg(test)]
#[path = "write_projection_tests.rs"]
mod tests;

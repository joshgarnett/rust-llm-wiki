//! Sealed admission for a bounded combined overlay of fresh source captures.
use super::{
    eligibility_facts::EligibilityFact,
    link_facts,
    normalized_delta::{DocumentMutation, OwnedDiagnostics, RevisionIdentityRow},
    normalized_fact_delta::{OwnedRegistryKeys, RecordFactMutation},
    query::QuerySnapshot,
    query_types::QueryCatalog,
    row_projection, scan,
    source_projection::{self, RefreshProjectionLimits, Work, baseline, conflict, deps, entry},
    structural_projection,
    types::RecordRow,
    write_projection::{ProjectedWrite, ProjectedWriteParts, empty_delta, project_policy},
};
use crate::{
    changes::{
        ChangeDraft,
        indexed_refresh::{IndexedCaptureTarget, IndexedWriteOperation},
    },
    domain::{
        Blake3Hash, Eligibility, ErrorCode, RecordId, RecordKind, Result, VaultRelativePath,
        WikiError,
    },
    sources::{SourceCaptureState, SourcePlan, SourceRefreshLookup},
    vault::{ExpectedState, VaultFs},
};
use std::collections::{BTreeMap, BTreeSet};

/// A SourcePlan is input, not authority. Validate its complete generated envelope
/// and selected dependencies before it can use the shared write publication.
pub(crate) fn project_capture(
    fs: &VaultFs,
    reader: &QuerySnapshot,
    plan: SourcePlan,
    limits: &RefreshProjectionLimits,
) -> Result<ProjectedWrite> {
    project_capture_core(fs, reader, vec![plan], limits, false)
}

/// A batch is one selected admission and one publication, never concatenated
/// independently sealed deltas. Size-one importer groups keep this descriptor;
/// the scalar entry point above retains the historical SourceCapture envelope.
pub(crate) fn project_capture_batch(
    fs: &VaultFs,
    reader: &QuerySnapshot,
    plans: Vec<SourcePlan>,
    limits: &RefreshProjectionLimits,
) -> Result<ProjectedWrite> {
    project_capture_core(fs, reader, plans, limits, true)
}

struct CaptureTree {
    source_id: RecordId,
    revision_id: RecordId,
    source_root: VaultRelativePath,
    source_path: VaultRelativePath,
    revision_path: VaultRelativePath,
    content_path: VaultRelativePath,
    content: Option<Vec<u8>>,
}

fn project_capture_core(
    fs: &VaultFs,
    reader: &QuerySnapshot,
    mut plans: Vec<SourcePlan>,
    limits: &RefreshProjectionLimits,
    batch: bool,
) -> Result<ProjectedWrite> {
    if plans.is_empty() || plans.len() > 8 {
        return Err(WikiError::invalid("capture group requires 1–8 fresh plans"));
    }
    plans.sort_by(|left, right| left.source_id.cmp(&right.source_id));
    let ids: BTreeSet<_> = plans
        .iter()
        .flat_map(|plan| [&plan.source_id, &plan.revision_id])
        .collect();
    if ids.len() != plans.len() * 2 {
        return Err(WikiError::invalid(
            "capture group Source and Revision identities overlap",
        ));
    }
    // The nominal group bound applies to original input bytes only. A scalar-
    // supported large item may form a size-one group; all actual Work/file/delta
    // bounds still apply to every group, including supplied extracted content.
    if plans.len() > 1 {
        let original_bytes = plans.iter().try_fold(0usize, |total, plan| {
            let original = plan
                .draft
                .as_ref()
                .and_then(|draft| {
                    draft
                        .operations
                        .iter()
                        .find(|operation| operation.target.as_str().ends_with("/original.bin"))
                })
                .and_then(|operation| operation.proposed.as_ref())
                .ok_or_else(|| WikiError::invalid("capture group lacks original input"))?;
            total.checked_add(original.len()).ok_or_else(|| {
                WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "capture group original input bound overflow",
                )
            })
        })?;
        if original_bytes > 4 * 1024 * 1024 {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "capture group original input exceeds 4 MiB",
            ));
        }
    }
    reader.require_policy_layout()?;
    let base = QueryCatalog::snapshot(reader).clone();
    if base.publication().is_none() {
        return Err(conflict("capture admission requires a pinned publication"));
    }
    let mut work = Work::new(fs, reader, limits)?;
    work.capture_path(&VaultRelativePath::new("WIKI.md")?)?;
    let mut trees = Vec::new();
    let mut drafts = Vec::new();
    for plan in plans {
        if plan.source_id == plan.revision_id
            || plan.reused
            || plan.invalidation != Default::default()
            || !plan.dependencies.is_empty()
        {
            return Err(WikiError::invalid(
                "capture requires one fresh generated source plan",
            ));
        }
        let draft = plan
            .draft
            .ok_or_else(|| WikiError::invalid("capture plan lacks generated operations"))?;
        validate_draft(&draft, &plan.source_id, &plan.revision_id)?;

        let source_path = VaultRelativePath::new(format!("sources/{}/source.md", plan.source_id))?;
        let source_root = VaultRelativePath::new(format!("sources/{}", plan.source_id))?;
        require_fresh_root(fs, &source_root)?;
        let revision_root = format!("sources/{}/revisions/{}", plan.source_id, plan.revision_id);
        let revision_path = VaultRelativePath::new(format!("{revision_root}/revision.md"))?;
        let original_path = VaultRelativePath::new(format!("{revision_root}/original.bin"))?;
        let content_path = VaultRelativePath::new(format!("{revision_root}/content.md"))?;
        // The indexed reservation probe covers claims, dangling typed references,
        // exact companion paths and policy identity dependencies for either kind.
        for (id, path) in [
            (&plan.source_id, &source_path),
            (&plan.revision_id, &revision_path),
        ] {
            if reader.revision_identity_is_reserved(id, path)? {
                return Err(conflict(
                    "generated capture identity or path is already reserved",
                ));
            }
        }
        let mut operations = BTreeMap::new();
        for operation in &draft.operations {
            work.tick()?;
            let bytes = operation
                .proposed
                .as_ref()
                .ok_or_else(|| WikiError::invalid("capture cannot delete files"))?;
            if operation.expected != ExpectedState::Absent
                || bytes.len() > limits.max_file_bytes
                || work.overlay.contains_key(&operation.target)
                || operations
                    .insert(operation.target.clone(), operation)
                    .is_some()
            {
                return Err(WikiError::invalid(
                    "capture operations are not fresh bounded targets",
                ));
            }
            work.charge(bytes.len())?;
            if reader.document_metadata(&operation.target)?.is_some()
                || reader.record_at_path(&operation.target)?.is_some()
            {
                return Err(conflict("capture target is already indexed"));
            }
            work.capture(&operation.target, &ExpectedState::Absent)?;
            work.overlay.insert(operation.target.clone(), bytes.clone());
        }
        let source_note = work.note(&source_path)?;
        let revision_note = work.note(&revision_path)?;
        let source = source_note
            .canonical
            .as_ref()
            .filter(|r| r.kind() == RecordKind::Source && r.id() == &plan.source_id)
            .ok_or_else(|| WikiError::invalid("capture lacks its generated Source envelope"))?;
        let revision = revision_note
            .canonical
            .as_ref()
            .filter(|r| r.kind() == RecordKind::Revision && r.id() == &plan.revision_id)
            .ok_or_else(|| WikiError::invalid("capture lacks its generated Revision envelope"))?;
        if !source_note.body().is_empty()
            || !revision_note.body().is_empty()
            || source.string("wiki_schema") != Some("1")
            || revision.string("wiki_schema") != Some("1")
            || source.fields().keys().any(|key| !source_field(key))
            || revision.fields().keys().any(|key| !revision_field(key))
            || source.title() != revision.title()
            || source.string("wiki_status") != Some("active")
            || source.string("wiki_current_revision") != Some(plan.revision_id.as_str())
            || source.string("wiki_revision") != Some(format!("[[{revision_path}]]").as_str())
            || scan::list(source, "wiki_revisions") != [plan.revision_id.to_string()]
            || revision.string("wiki_source_id") != Some(plan.source_id.as_str())
            || revision.string("wiki_source") != Some(format!("[[{source_path}]]").as_str())
            || revision.string("wiki_original_path") != Some("original.bin")
        {
            return Err(WikiError::invalid(
                "capture differs from the generated ownership envelope",
            ));
        }
        if revision.field("origin_retrieved_at").is_some()
            || revision.field("origin_retrieved_at_kind").is_some()
        {
            if source.string("wiki_origin_kind") != Some("agent-report")
                || revision.string("origin_retrieved_at_kind") != Some("agent-claimed")
                || revision.string("origin_retrieved_at").is_none_or(|value| {
                    time::OffsetDateTime::parse(
                        value,
                        &time::format_description::well_known::Rfc3339,
                    )
                    .is_err()
                })
            {
                return Err(WikiError::invalid(
                    "capture has invalid agent retrieval provenance",
                ));
            }
        }
        let complete = revision.string("wiki_extraction_status") == Some("complete");
        let expected_capture_state = if complete {
            if revision.string("wiki_content_path") != Some("content.md") {
                return Err(WikiError::invalid(
                    "complete capture lacks its generated content path",
                ));
            }
            let bytes = work
                .overlay
                .get(&content_path)
                .ok_or_else(|| WikiError::invalid("complete capture lacks content bytes"))?;
            if bytes.is_empty() {
                SourceCaptureState::Empty
            } else {
                SourceCaptureState::Complete
            }
        } else {
            if revision.string("wiki_extraction_status") != Some("unsupported")
                || revision.field("wiki_content_path").is_some()
                || revision.field("wiki_content_hash").is_some()
            {
                return Err(WikiError::invalid(
                    "unsupported capture cannot claim extracted content",
                ));
            }
            SourceCaptureState::Unsupported
        };
        if plan.capture_state != Some(expected_capture_state) {
            return Err(WikiError::invalid(
                "capture state differs from generated extracted bytes",
            ));
        }
        let mut required_paths = BTreeSet::from([
            source_path.clone(),
            revision_path.clone(),
            original_path.clone(),
        ]);
        let mut asset_paths = BTreeSet::from([original_path.clone()]);
        if complete {
            required_paths.insert(content_path.clone());
            asset_paths.insert(content_path.clone());
        }
        if operations.keys().cloned().collect::<BTreeSet<_>>() != required_paths {
            return Err(WikiError::invalid(
                "capture writes outside one exact immutable tree",
            ));
        }
        for (path, operation) in &operations {
            let required = if path == &source_path {
                required_paths
                    .difference(&BTreeSet::from([source_path.clone()]))
                    .cloned()
                    .collect()
            } else if path == &revision_path {
                asset_paths.clone()
            } else {
                BTreeSet::new()
            };
            if operation
                .apply_after
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>()
                != required
                || operation.apply_after.len() != required.len()
            {
                return Err(WikiError::invalid(
                    "capture write ordering differs from generated ownership",
                ));
            }
        }
        // Immutable assets are proof inputs only; they are never parsed into a
        // canonical record or accepted as a caller-supplied graph overlay.
        for (record, path, hash, direct_paths) in [
            (
                source.clone(),
                source_path.clone(),
                source_note.source_hash.clone(),
                BTreeSet::from([source_path.clone()]),
            ),
            (
                revision.clone(),
                revision_path.clone(),
                revision_note.source_hash.clone(),
                asset_paths
                    .iter()
                    .cloned()
                    .chain(std::iter::once(revision_path.clone()))
                    .collect(),
            ),
        ] {
            let row = RecordRow {
                authored_status: record.string("wiki_status").map(str::to_owned),
                record,
                path: path.clone(),
                hash,
                eligibility: Eligibility::Current,
                reasons: vec![],
                identity_eligibility: None,
                description_eligibility: None,
                disputed: false,
                dependencies: vec![],
            };
            let id = row.record.id().clone();
            work.new_registry.push(entry(&row));
            work.replaced_registry.insert(path);
            work.facts.insert(
                id.clone(),
                EligibilityFact {
                    baseline: baseline(),
                    structural: Default::default(),
                    direct_paths,
                },
            );
            work.now.insert(id, row);
        }
        let content = work.verify_head(&plan.source_id, &plan.revision_id)?;
        trees.push(CaptureTree {
            source_id: plan.source_id,
            revision_id: plan.revision_id,
            source_root,
            source_path,
            revision_path,
            content_path,
            content,
        });
        drafts.push(draft);
    }
    let mut draft = drafts.remove(0);
    if batch {
        draft.title = format!("Capture {} sources", trees.len());
        draft.allocated_ids = trees
            .iter()
            .enumerate()
            .flat_map(|(index, tree)| {
                [
                    (format!("source_{index}"), tree.source_id.clone()),
                    (format!("revision_{index}"), tree.revision_id.clone()),
                ]
            })
            .collect();
    }
    for remaining in drafts {
        draft.operations.extend(remaining.operations);
    }
    let revision_ids: BTreeSet<_> = trees.iter().map(|tree| tree.revision_id.clone()).collect();
    let mut seeds = trees
        .iter()
        .flat_map(|tree| [tree.source_id.clone(), tree.revision_id.clone()])
        .collect::<BTreeSet<_>>();
    let keys: Vec<_> = work
        .new_registry
        .iter()
        .map(link_facts::registry_keys)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let affected_links = reader.affected_links(&keys)?;
    for (path, offset) in &affected_links {
        if reader
            .link_fact(path, *offset)?
            .is_some_and(|fact| fact.typed.is_some())
        {
            let owner = reader
                .document_metadata(path)?
                .and_then(|row| row.record_id)
                .ok_or_else(|| {
                    source_projection::corrupt("typed capture navigation lacks an owner")
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
    // Lifecycle discovery can rebind authenticated generation-output packets
    // for an affected navigation owner. Preserve the shared Page edge rule.
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
        let facts = delta.facts.as_mut().unwrap();
        facts.edge_deletes.extend(old.difference(&new).cloned());
        facts.edge_inserts.extend(new.difference(&old).cloned());
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
                "capture invalidates typed record {id}: {}",
                row.reasons.join(",")
            )));
        }
    }
    work.admitted_typed_navigation
        .extend(structural.iter().map(|id| work.now[id].path.clone()));
    for id in work.dynamic.clone() {
        work.emit_record(&id, &mut delta)?;
        let row = work.now[&id].clone();
        if row.record.kind() == RecordKind::Revision
            && !revision_ids.contains(row.record.id())
            && row.record.string("wiki_extraction_status") == Some("complete")
        {
            let path = source_projection::asset_path(
                &row,
                row.record.string("wiki_content_path").unwrap(),
            )?;
            if reader.document_metadata(&path)?.is_some() {
                work.metadata_document(
                    &path,
                    &row,
                    None,
                    Some((
                        source_projection::record_id(&row.record, "wiki_source_id")?,
                        id,
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
            let prior = reader
                .eligibility_fact(&id)?
                .ok_or_else(|| source_projection::corrupt("capture old structural fact missing"))?
                .structural
                .diagnostics(old);
            diagnostics.retain(|diagnostic| !prior.contains(diagnostic));
        }
        diagnostics.extend(fact.structural.diagnostics(row));
        diagnostics.retain(|diagnostic| {
            diagnostic
                .details
                .get("reason")
                .and_then(serde_json::Value::as_str)
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
    for tree in &mut trees {
        let head = &work.now[&tree.revision_id];
        delta.revisions.push(RevisionIdentityRow {
            source_id: tree.source_id.clone(),
            revision_id: tree.revision_id.clone(),
            retained_ordinal: 0,
            original_hash: Blake3Hash::new(head.record.string("wiki_original_hash").unwrap())?,
            content_hash: head
                .record
                .string("wiki_content_hash")
                .map(Blake3Hash::new)
                .transpose()?,
            extractor_fingerprint: Blake3Hash::new(
                head.record.string("wiki_extractor_fingerprint").unwrap(),
            )?,
            extraction_status: head.record.string("wiki_extraction_status").unwrap().into(),
        });
        if let Some(content) = tree.content.take() {
            let text = String::from_utf8(content)
                .map_err(|_| conflict("captured content is not UTF-8"))?;
            delta.documents.push(DocumentMutation::Put {
                row: row_projection::captured_content_document(
                    tree.content_path.clone(),
                    tree.source_id.clone(),
                    head,
                    text,
                ),
            });
        }
    }
    for registry in &work.new_registry {
        delta
            .facts
            .as_mut()
            .unwrap()
            .registry
            .push(OwnedRegistryKeys {
                record_id: registry.id.clone(),
                path: registry.path.clone(),
                keys: link_facts::registry_keys(registry)?,
            });
    }
    let mut link_owners = trees
        .iter()
        .flat_map(|tree| [tree.source_path.clone(), tree.revision_path.clone()])
        .collect::<BTreeSet<_>>();
    link_owners.extend(affected_links.into_iter().map(|(path, _)| path));
    for path in link_owners {
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
    // Directory creation is not representable by a file-hash ReadDependency.
    // Preparation independently repeats this new-root check before allocation.
    for tree in &trees {
        require_fresh_root(fs, &tree.source_root)?;
    }
    let operation = if batch {
        IndexedWriteOperation::SourceCaptureBatch {
            captures: trees
                .iter()
                .map(|tree| IndexedCaptureTarget {
                    source_id: tree.source_id.clone(),
                    revision_id: tree.revision_id.clone(),
                })
                .collect(),
        }
    } else {
        IndexedWriteOperation::SourceCapture {
            source_id: trees[0].source_id.clone(),
            revision_id: trees[0].revision_id.clone(),
        }
    };
    operation.validate()?;
    Ok(ProjectedWrite::from_parts(ProjectedWriteParts {
        operation,
        draft,
        base,
        before: deps(work.before),
        after: deps(after),
        delta,
    }))
}

fn require_fresh_root(fs: &VaultFs, root: &VaultRelativePath) -> Result<()> {
    match std::fs::symlink_metadata(fs.root().resolve(root)?) {
        Ok(_) => Err(conflict("generated source root already exists")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(WikiError::new(
            crate::domain::ErrorCode::Internal,
            format!("inspect generated source root: {error}"),
        )),
    }
}

fn validate_draft(
    draft: &ChangeDraft,
    source: &crate::domain::RecordId,
    revision: &crate::domain::RecordId,
) -> Result<()> {
    if draft.origin.is_some()
        || draft.inverse_of.is_some()
        || !draft.read_preconditions.is_empty()
        || draft.operations.len() < 3
        || draft.operations.len() > 4
        || draft.title.trim().is_empty()
        || draft.title.len() > 16_384
        || draft.allocated_ids
            != BTreeMap::from([
                ("source".into(), source.clone()),
                ("revision".into(), revision.clone()),
            ])
    {
        return Err(WikiError::invalid(
            "capture draft differs from one fresh generator allocation",
        ));
    }
    Ok(())
}

fn source_field(name: &str) -> bool {
    matches!(
        name,
        "wiki_schema"
            | "wiki_id"
            | "wiki_kind"
            | "title"
            | "wiki_status"
            | "wiki_origin_kind"
            | "wiki_origin"
            | "wiki_current_revision"
            | "wiki_revision"
            | "wiki_revisions"
    )
}
fn revision_field(name: &str) -> bool {
    matches!(
        name,
        "wiki_schema"
            | "wiki_id"
            | "wiki_kind"
            | "title"
            | "wiki_source_id"
            | "wiki_source"
            | "wiki_captured_at"
            | "wiki_original_path"
            | "wiki_original_hash"
            | "wiki_extractor"
            | "wiki_extractor_fingerprint"
            | "wiki_extraction_status"
            | "wiki_media_type"
            | "wiki_content_path"
            | "wiki_content_hash"
            | "origin_retrieved_at"
            | "origin_retrieved_at_kind"
    )
}

#[cfg(test)]
#[path = "capture_projection_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "capture_batch_projection_tests.rs"]
mod batch_tests;

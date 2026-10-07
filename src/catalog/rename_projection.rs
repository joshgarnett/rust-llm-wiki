//! One admitted Page path transition, with its complete selected navigation.
//! Candidate match keys discover owners; authenticated before bytes authorize
//! edits. No full projection or canonical membership scan occurs here.
use super::{
    eligibility_facts::EligibilityRole,
    link_facts,
    navigation_resolution::{self, NavigationResolution},
    normalized_delta::{DocumentMutation, OwnedClaims, OwnedDiagnostics, OwnedLinks},
    normalized_fact_delta::{OwnedLinkFacts, OwnedRegistryKeys, RecordFactMutation},
    query::QuerySnapshot,
    query_types::QueryCatalog,
    row_projection, scan,
    source_projection::{self, RefreshProjectionLimits, Work, budget, conflict, deps, entry},
    structural_projection,
    types::IdentityClaimRow,
    write_projection::{ProjectedWrite, ProjectedWriteParts, empty_delta, project_policy_move},
};
use crate::{
    changes::{ChangeDraft, ExpectedWrite, indexed_refresh::IndexedWriteOperation},
    domain::{Blake3Hash, Eligibility, RecordId, RecordKind, Result, VaultRelativePath, WikiError},
    records::{
        LinkResolution, edit_note,
        link_rewrite::{rewrite_companions_selected, rewrite_links_selected},
        parse_note,
    },
    vault::{ExpectedState, VaultFs},
};
use std::collections::{BTreeMap, BTreeSet};

fn immutable(path: &VaultRelativePath, bytes: &[u8]) -> bool {
    let parts: Vec<_> = path.as_str().split('/').collect();
    let revision_tree = parts.len() >= 4
        && unicase::UniCase::unicode(parts[0]).to_folded_case() == "sources"
        && unicase::UniCase::unicode(parts[2]).to_folded_case() == "revisions";
    revision_tree
        || parse_note(bytes).fields.is_some_and(|fields| {
            fields
                .get("wiki_kind")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|kind| matches!(kind, "revision" | "extraction_packet" | "run_event"))
        })
}

fn link_resolution(value: NavigationResolution) -> LinkResolution {
    match value {
        NavigationResolution::Resolved {
            id,
            path,
            fragment,
            companion_stale,
        } => LinkResolution::Resolved {
            id,
            path,
            fragment,
            companion_stale,
        },
        NavigationResolution::Missing => LinkResolution::Missing,
        NavigationResolution::External => LinkResolution::External,
        NavigationResolution::Ambiguous => LinkResolution::Ambiguous { ids: vec![] },
        NavigationResolution::WrongKind { actual } => LinkResolution::WrongKind { actual },
        NavigationResolution::CompanionConflict { expected, actual } => {
            LinkResolution::CompanionConflict { expected, actual }
        }
    }
}

pub(crate) fn project_page_rename(
    fs: &VaultFs,
    reader: &QuerySnapshot,
    page_id: RecordId,
    to: VaultRelativePath,
    if_match: Blake3Hash,
    limits: &RefreshProjectionLimits,
) -> Result<Option<ProjectedWrite>> {
    reader.require_policy_layout()?;
    let base = QueryCatalog::snapshot(reader).clone();
    if base.publication().is_none() {
        return Err(conflict("Page rename requires a pinned publication"));
    }
    let mut work = Work::new(fs, reader, limits)?;
    work.capture_path(&VaultRelativePath::new("WIKI.md")?)?;
    if !work.load(&page_id)? {
        return Err(WikiError::new(
            crate::domain::ErrorCode::RecordNotFound,
            format!("Page rename target not found: {page_id}"),
        ));
    }
    let old = work.old[&page_id].clone();
    let from = old.path.clone();
    let claim = reader.unique_identity_claim(&page_id)?;
    if old.record.kind() != RecordKind::Page
        || old.hash != if_match
        || claim.as_ref().is_none_or(|claim| {
            claim.path != from || claim.hash != if_match || claim.kind != Some(RecordKind::Page)
        })
    {
        return Err(conflict(
            "rename requires the exact author hash of a uniquely adopted Page",
        ));
    }
    if !crate::sources::revision::canonical_path(&to)
        || immutable(&from, &work.captured[&from].bytes)
        || immutable(&to, &[])
    {
        return Err(WikiError::invalid(
            "Page rename cannot move reserved or immutable paths",
        ));
    }
    if from == to {
        work.recheck()?;
        return Ok(None);
    }
    fs.root()
        .validate_portable_paths(&[from.clone(), to.clone()])?;
    work.capture(&to, &ExpectedState::Absent)?;
    if reader.document_metadata(&to)?.is_some()
        || crate::sources::SourceRefreshLookup::record_at_path(reader, &to)?.is_some()
    {
        return Err(conflict(
            "rename destination is already owned by the publication",
        ));
    }

    let mut moved = old.clone();
    moved.path = to.clone();
    let new_entry = entry(&moved);
    let mut keys: BTreeSet<_> = link_facts::registry_keys(&entry(&old))?
        .into_iter()
        .collect();
    keys.extend(link_facts::registry_keys(&new_entry)?);
    let keys: Vec<_> = keys.into_iter().collect();
    let affected = reader.affected_links(&keys)?;
    let mut typed_owners = BTreeSet::new();
    for (path, offset) in &affected {
        let fact = reader
            .link_fact(path, *offset)?
            .ok_or_else(|| source_projection::corrupt("affected link fact missing"))?;
        if fact.typed.is_some() {
            typed_owners.insert(path.clone());
        }
    }
    let mut owners: BTreeSet<_> = affected.iter().map(|(path, _)| path.clone()).collect();
    owners.insert(from.clone());
    let mut seeds = BTreeSet::from([page_id.clone()]);
    let mut rewrites = BTreeMap::new();
    let mut plain = BTreeSet::new();
    for path in &owners {
        work.tick()?;
        let metadata = reader
            .document_metadata(path)?
            .ok_or_else(|| source_projection::corrupt("incoming navigation owner missing"))?;
        if metadata.owner_revision.is_some() {
            return Err(source_projection::corrupt(
                "captured payload is not a canonical navigation owner",
            ));
        }
        if typed_owners.contains(path) {
            let id = metadata
                .record_id
                .as_ref()
                .ok_or_else(|| source_projection::corrupt("typed incoming owner is not adopted"))?;
            work.require(id)?;
            seeds.insert(id.clone());
        }
        work.capture_path(path)?;
        let note = work.note(path)?;
        let body = std::str::from_utf8(note.body())
            .map_err(|_| conflict("indexed incoming body is no longer UTF-8"))?;
        let mut resolve = |destination: &str| {
            navigation_resolution::resolve_untyped(destination, &mut |key| {
                work.registry_probe(key, false)
            })
            .map(link_resolution)
        };
        let changed_body = rewrite_links_selected(body, &mut resolve, &page_id, &to)?;
        let fields = if metadata.record_id.is_some() {
            rewrite_companions_selected(&note, &mut resolve, &page_id, &to)?
        } else {
            BTreeMap::new()
        };
        if changed_body == note.body() && fields.is_empty() {
            continue;
        }
        if immutable(path, &note.raw) {
            return Err(WikiError::invalid(format!(
                "immutable incoming navigation owner cannot be rewritten: {path}"
            )));
        }
        let bytes = if note.canonical.is_some() {
            edit_note(&note, &fields, Some(&changed_body), &note.source_hash)?
        } else if fields.is_empty() {
            let mut bytes = note.raw[..note.raw.len() - note.body().len()].to_vec();
            bytes.extend(changed_body);
            bytes
        } else {
            return Err(WikiError::invalid(
                "unadopted incoming note cannot own typed edits",
            ));
        };
        work.charge(bytes.len())?;
        if let Some(id) = metadata.record_id {
            work.require(&id)?;
            let before = &work.old[&id];
            let after = parse_note(&bytes)
                .canonical
                .ok_or_else(|| conflict("navigation rewrite invalidates adopted envelope"))?;
            if before.record.id() != after.id()
                || before.record.kind() != after.kind()
                || before.path != *path
                || source_projection::entry(before).aliases != scan::list(&after, "aliases")
            {
                return Err(conflict(
                    "incoming rewrite changes identity, kind, path or aliases",
                ));
            }
            let mut now = before.clone();
            now.record = after;
            now.hash = Blake3Hash::digest(&bytes);
            work.now.insert(id.clone(), now);
            seeds.insert(id);
        } else {
            plain.insert(path.clone());
        }
        rewrites.insert(path.clone(), bytes);
    }
    let destination_bytes = rewrites
        .remove(&from)
        .unwrap_or_else(|| work.captured[&from].bytes.clone());
    let destination_bytes =
        crate::app::page_citations::rebase_source_citation_links(&destination_bytes, &from, &to)?;
    work.charge(destination_bytes.len())?;
    moved.hash = Blake3Hash::digest(&destination_bytes);
    moved.record = parse_note(&destination_bytes)
        .canonical
        .ok_or_else(|| conflict("moved Page envelope became invalid"))?;
    work.now.insert(page_id.clone(), moved);
    let direct_paths = &mut work.facts.get_mut(&page_id).unwrap().direct_paths;
    if !direct_paths.remove(&from) || !direct_paths.insert(to.clone()) {
        return Err(source_projection::corrupt(
            "moved Page verification fact lacks its unique old path",
        ));
    }
    work.removed_paths.insert(from.clone());
    work.overlay.insert(to.clone(), destination_bytes.clone());
    work.overlay.extend(rewrites.clone());
    work.replaced_registry.insert(from.clone());
    work.new_registry.push(new_entry);
    // Rewrite decisions used the before registry. After replacement, cached
    // buckets must be reprobed with the old owner excluded; otherwise the same
    // ID at old and new paths falsely becomes ambiguous.
    work.key_cache.clear();
    let policy = project_policy_move(&mut work)?;
    let mut delta = empty_delta(policy.clone());
    let structural = structural_projection::recompute(&mut work, seeds, &policy, &mut delta)?;
    work.discover(structural.clone())?;
    work.recompute()?;
    for id in &structural {
        let prior: BTreeSet<_> = reader
            .outgoing_edges(id, &[EligibilityRole::GenerationPacket])?
            .into_iter()
            .collect();
        let next: BTreeSet<_> = work.edge_overrides[id]
            .iter()
            .filter(|edge| edge.role == EligibilityRole::GenerationPacket)
            .cloned()
            .collect();
        delta
            .facts
            .as_mut()
            .unwrap()
            .edge_deletes
            .extend(prior.difference(&next).cloned());
        delta
            .facts
            .as_mut()
            .unwrap()
            .edge_inserts
            .extend(next.difference(&prior).cloned());
    }
    for id in &work.dynamic {
        let row = &work.now[id];
        if row.eligibility == Eligibility::Invalid
            && (work.overlay.contains_key(&row.path)
                || work.old.get(id).is_none_or(|before| {
                    before.eligibility != Eligibility::Invalid || before.reasons != row.reasons
                }))
        {
            return Err(WikiError::invalid(format!(
                "rename invalidates typed record {id}"
            )));
        }
    }
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
                        id,
                    )),
                    &mut delta,
                )?;
            }
        }
    }
    for id in &structural {
        let row = &work.now[id];
        let fact = &work.facts[id];
        delta
            .facts
            .as_mut()
            .unwrap()
            .records
            .push(RecordFactMutation {
                record_id: id.clone(),
                fact: fact.clone(),
            });
        let old_path = work.old.get(id).map_or(&row.path, |old| &old.path);
        let mut diagnostics = reader.diagnostics(&BTreeSet::from([old_path.clone()]))?;
        if let Some(old) = work.old.get(id) {
            let prior = reader
                .eligibility_fact(id)?
                .ok_or_else(|| source_projection::corrupt("old structural fact missing"))?
                .structural
                .diagnostics(old);
            diagnostics.retain(|diagnostic| !prior.contains(diagnostic));
        }
        for diagnostic in &mut diagnostics {
            diagnostic.path = row.path.clone();
        }
        diagnostics.extend(fact.structural.diagnostics(row));
        diagnostics.retain(|diagnostic| {
            diagnostic
                .details
                .get("reason")
                .and_then(serde_json::Value::as_str)
                != Some("generation_output_source_unresolved")
        });
        if let Some(Some(diagnostic)) = work.lifecycle_diagnostics.get(id) {
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
    for path in &plain {
        let note = work.note(path)?;
        delta.documents.push(DocumentMutation::Put {
            row: row_projection::canonical_document(path, &note, None),
        });
        delta.claims.push(OwnedClaims {
            path: path.clone(),
            rows: crate::sources::identity::readable_ids(&note)
                .into_iter()
                .map(|id| IdentityClaimRow {
                    id,
                    path: path.clone(),
                    hash: note.source_hash.clone(),
                    kind: note.canonical.as_ref().map(|record| record.kind()),
                })
                .collect(),
        });
        delta.diagnostics.push(OwnedDiagnostics {
            path: path.clone(),
            rows: reader.diagnostics(&BTreeSet::from([path.clone()]))?,
        });
    }
    let row = work.now[&page_id].clone();
    let position = delta
        .documents
        .iter()
        .position(|mutation| {
            matches!(mutation,
        DocumentMutation::Put { row } if row.path == to && row.record_id.as_ref() == Some(&page_id))
        })
        .ok_or_else(|| source_projection::corrupt("moved Page document was not emitted"))?;
    delta.documents[position] = DocumentMutation::MovePage {
        from: from.clone(),
        from_hash: if_match.clone(),
        row: row_projection::canonical_document(&to, &work.note(&to)?, Some(&row)),
    };
    // Retire rows owned by the old note. References whose lookup values still
    // mention this path belong to their incoming owners and remain intact.
    delta.claims.push(OwnedClaims {
        path: from.clone(),
        rows: vec![],
    });
    delta.diagnostics.push(OwnedDiagnostics {
        path: from.clone(),
        rows: vec![],
    });
    delta.links.push(OwnedLinks {
        path: from.clone(),
        rows: vec![],
    });
    delta.facts.as_mut().unwrap().links.push(OwnedLinkFacts {
        path: from.clone(),
        rows: vec![],
    });
    delta
        .facts
        .as_mut()
        .unwrap()
        .registry
        .push(OwnedRegistryKeys {
            record_id: page_id.clone(),
            path: to.clone(),
            keys: link_facts::registry_keys(&entry(&row))?,
        });
    let mut navigation_owners = owners;
    navigation_owners.remove(&from);
    navigation_owners.insert(to.clone());
    for path in navigation_owners {
        work.emit_links(&path, &mut delta)?;
    }
    let mut assertions: BTreeSet<_> = reader
        .affected_assertion_navigation(&keys)?
        .into_iter()
        .collect();
    assertions.extend(
        work.dynamic
            .iter()
            .filter(|id| work.now[*id].record.kind() == RecordKind::Assertion)
            .cloned(),
    );
    for id in assertions {
        work.emit_assertion_navigation(&id, &mut delta)?;
    }
    let rewritten_paths: Vec<_> = rewrites.keys().cloned().collect();
    let mut operations = vec![ExpectedWrite {
        target: to.clone(),
        expected: ExpectedState::Absent,
        proposed: Some(destination_bytes),
        apply_after: vec![],
    }];
    for (path, bytes) in rewrites {
        operations.push(ExpectedWrite {
            expected: work.before[&path].clone(),
            target: path,
            proposed: Some(bytes),
            apply_after: vec![to.clone()],
        });
    }
    let mut removal_after = rewritten_paths.clone();
    removal_after.push(to.clone());
    operations.push(ExpectedWrite {
        target: from.clone(),
        expected: ExpectedState::Hash(if_match.clone()),
        proposed: None,
        apply_after: removal_after,
    });
    if operations.len() > crate::changes::prepare::MAX_OPS {
        return Err(budget());
    }
    let mut after = work.before.clone();
    after.insert(from.clone(), ExpectedState::Absent);
    for (path, bytes) in &work.overlay {
        after.insert(path.clone(), ExpectedState::Hash(Blake3Hash::digest(bytes)));
    }
    // The old absence remains in the retained proof. MovePage's SQL handler
    // retires its obsolete logical dependency after validating this guard.
    delta.dependencies = deps(after.clone());
    delta.validate()?;
    work.recheck()?;
    Ok(Some(ProjectedWrite::from_parts(ProjectedWriteParts {
        operation: IndexedWriteOperation::PageRename {
            page_id,
            from,
            to,
            from_hash: if_match,
            rewritten_paths,
        },
        draft: ChangeDraft {
            title: "Rename authored Page and known navigation".into(),
            origin: None,
            inverse_of: None,
            allocated_ids: BTreeMap::new(),
            read_preconditions: deps(work.before.clone()),
            operations,
        },
        base,
        before: deps(work.before),
        after: deps(after),
        delta,
    })))
}

#[cfg(test)]
#[path = "rename_projection_tests.rs"]
mod tests;

//! Selected source withdrawal: one mutable envelope, immutable history retained.
use super::{
    eligibility_facts::EligibilityRole,
    normalized_delta::{CatalogDelta, OwnedDiagnostics},
    normalized_fact_delta::RecordFactMutation,
    query::QuerySnapshot,
    query_types::QueryCatalog,
    scan,
    source_projection::{
        RefreshProjectionLimits, Work, asset_path, conflict, corrupt, deps, record_id, typed,
    },
    structural_projection,
    write_projection::{ProjectedWrite, ProjectedWriteParts, empty_delta, project_policy},
};
use crate::{
    changes::{ChangeDraft, ExpectedWrite, indexed_refresh::IndexedWriteOperation},
    domain::{
        Blake3Hash, Eligibility, ErrorCode, RecordId, RecordKind, Result, VaultRelativePath,
        WikiError,
    },
    records::edit_note,
    sources::SourceRefreshLookup,
    vault::{ExpectedState, VaultFs},
};
use std::collections::{BTreeMap, BTreeSet};

const MAX_REASON_BYTES: usize = 4096;

/// None is an authenticated no-op. It does not create a changeset or epoch.
pub(crate) fn project_withdraw(
    fs: &VaultFs,
    reader: &QuerySnapshot,
    source_id: &RecordId,
    reason: &str,
    limits: &RefreshProjectionLimits,
) -> Result<Option<ProjectedWrite>> {
    if reason.trim().is_empty() || reason.len() > MAX_REASON_BYTES {
        return Err(WikiError::invalid(
            "withdrawal requires a nonempty reason of at most 4096 bytes",
        ));
    }
    reader.require_policy_layout()?;
    let base = QueryCatalog::snapshot(reader).clone();
    if base.publication().is_none() {
        return Err(conflict("withdrawal requires a pinned publication"));
    }
    let mut work = Work::new(fs, reader, limits)?;
    work.capture_path(&VaultRelativePath::new("WIKI.md")?)?;
    let selected = reader.unique_record(source_id)?.ok_or_else(|| {
        WikiError::new(
            ErrorCode::RecordNotFound,
            "withdrawal source identity not found",
        )
    })?;
    if selected.record.kind() != RecordKind::Source {
        return Err(WikiError::invalid("withdrawal requires a Source identity"));
    }
    work.require(source_id)?;
    let original = work.now[source_id].clone();
    if original.path != selected.path || original.hash != selected.hash {
        return Err(corrupt(
            "withdrawal source claim differs from its adopted record",
        ));
    }
    match original.record.string("wiki_status") {
        Some("withdrawn") => {
            work.recheck()?;
            return Ok(None);
        }
        Some("active") => {}
        _ => {
            return Err(conflict(
                "withdrawal source has an unsupported lifecycle state",
            ));
        }
    }
    let note = work.note(&original.path)?;
    let fields = BTreeMap::from([
        ("wiki_status".into(), "withdrawn".into()),
        (
            "wiki_withdrawn_at".into(),
            crate::sources::revision::timestamp()?.into(),
        ),
        ("wiki_withdrawal_reason".into(), reason.into()),
    ]);
    let bytes = edit_note(&note, &fields, None, &original.hash)?;
    work.charge(bytes.len())?;
    if bytes.len() > limits.max_file_bytes {
        return Err(super::source_projection::budget());
    }
    work.overlay.insert(original.path.clone(), bytes.clone());
    let after_note = work.note(&original.path)?;
    let record = after_note
        .canonical
        .as_ref()
        .ok_or_else(|| conflict("withdrawal produced invalid metadata"))?;
    let fixed = |key: &&String| {
        !matches!(
            key.as_str(),
            "wiki_status" | "wiki_withdrawn_at" | "wiki_withdrawal_reason"
        )
    };
    if record.id() != source_id
        || record.kind() != RecordKind::Source
        || record
            .fields()
            .iter()
            .filter(|(key, _)| fixed(key))
            .ne(original
                .record
                .fields()
                .iter()
                .filter(|(key, _)| fixed(key)))
        || note.body() != after_note.body()
        || record.string("wiki_status") != Some("withdrawn")
        || record.string("wiki_withdrawal_reason") != Some(reason)
    {
        return Err(conflict("withdrawal changes fixed source fields or body"));
    }
    let mut replacement = original.clone();
    replacement.record = record.clone();
    replacement.hash = after_note.source_hash;
    replacement.authored_status = Some("withdrawn".into());
    work.now.insert(source_id.clone(), replacement);

    let revisions = lifecycle_seeds(&mut work, source_id)?;
    let mut seeds = BTreeSet::from([source_id.clone()]);
    seeds.extend(revisions.iter().cloned());
    // Source relationships cover current and historical evidence/packets and
    // extractions. Revision edges additionally find multi-source extractions.
    for edge in work.edges(
        source_id,
        &[typed("wiki_source_id"), EligibilityRole::ExtractionSource],
        true,
    )? {
        work.require(&edge.owner_id)?;
        seeds.insert(edge.owner_id);
    }
    for revision in &revisions {
        for edge in work.edges(
            revision,
            &[
                typed("wiki_source_revision"),
                EligibilityRole::ExtractionRevision,
            ],
            true,
        )? {
            work.require(&edge.owner_id)?;
            seeds.insert(edge.owner_id);
        }
    }
    for packet in seeds.clone() {
        if work.now[&packet].record.kind() == RecordKind::ExtractionPacket {
            for edge in work.edges(&packet, &[EligibilityRole::GenerationPacket], true)? {
                work.require(&edge.owner_id)?;
                seeds.insert(edge.owner_id);
            }
        }
    }
    let policy = project_policy(&mut work)?;
    let mut delta = empty_delta(policy.clone());
    let structural = structural_projection::recompute(&mut work, seeds, &policy, &mut delta)?;
    work.discover(structural.clone())?;
    work.recompute()?;
    for id in &work.dynamic {
        let row = &work.now[id];
        if row.eligibility == Eligibility::Invalid
            && work.old.get(id).is_none_or(|old| {
                old.eligibility != Eligibility::Invalid || old.reasons != row.reasons
            })
        {
            return Err(WikiError::invalid(format!(
                "withdrawal invalidates typed record {id}: {}",
                row.reasons.join(",")
            )));
        }
    }
    for id in work.dynamic.clone() {
        work.emit_record(&id, &mut delta)?;
        let row = work.now[&id].clone();
        if row.record.kind() == RecordKind::Revision
            && row.record.string("wiki_extraction_status") == Some("complete")
        {
            let content = asset_path(
                &row,
                row.record
                    .string("wiki_content_path")
                    .ok_or_else(|| corrupt("complete revision lacks content path"))?,
            )?;
            if reader.document_metadata(&content)?.is_some() {
                // Authenticated immutable Revision metadata binds this update;
                // do not read all historical content or original payloads.
                work.metadata_document(
                    &content,
                    &row,
                    None,
                    Some((record_id(&row.record, "wiki_source_id")?, id)),
                    &mut delta,
                )?;
            } else if row.eligibility != Eligibility::Invalid {
                return Err(corrupt(
                    "valid complete revision lacks captured content document",
                ));
            }
        }
    }
    emit_structural(&mut work, structural, &mut delta)?;
    work.emit_links(&original.path, &mut delta)?;
    let mut after = work.before.clone();
    after.insert(
        original.path.clone(),
        ExpectedState::Hash(Blake3Hash::digest(&bytes)),
    );
    let draft = ChangeDraft {
        title: format!("Withdraw {}", original.record.title()),
        origin: None,
        inverse_of: None,
        allocated_ids: BTreeMap::new(),
        read_preconditions: deps(work.before.clone()),
        operations: vec![ExpectedWrite {
            target: original.path,
            expected: ExpectedState::Hash(original.hash),
            proposed: Some(bytes),
            apply_after: vec![],
        }],
    };
    delta.dependencies = deps(after.clone());
    delta.validate()?;
    work.recheck()?;
    Ok(Some(ProjectedWrite::from_parts(ProjectedWriteParts {
        operation: IndexedWriteOperation::SourceWithdraw {
            source_id: source_id.clone(),
        },
        draft,
        base,
        before: deps(work.before),
        after: deps(after),
        delta,
    })))
}

fn lifecycle_seeds(work: &mut Work<'_>, source_id: &RecordId) -> Result<BTreeSet<RecordId>> {
    let source = work.now[source_id].clone();
    let inventory = scan::list(&source.record, "wiki_revisions");
    let revisions: BTreeSet<_> = inventory.iter().map(RecordId::new).collect::<Result<_>>()?;
    if inventory.len() != revisions.len()
        || !revisions.contains(&record_id(&source.record, "wiki_current_revision")?)
    {
        return Err(corrupt(
            "withdrawal source inventory is duplicate or lacks head",
        ));
    }
    let edges = work.edges(source_id, &[EligibilityRole::SourceInventory], false)?;
    if edges
        .iter()
        .map(|edge| &edge.target_id)
        .collect::<BTreeSet<_>>()
        != revisions.iter().collect()
    {
        return Err(corrupt("withdrawal source inventory edges are incomplete"));
    }
    let head = record_id(&source.record, "wiki_current_revision")?;
    let heads = work.edges(source_id, &[typed("wiki_current_revision")], false)?;
    if heads.len() != 1 || heads[0].target_id != head {
        return Err(corrupt(
            "withdrawal source head relation differs from inventory",
        ));
    }
    for revision in &revisions {
        work.require(revision)?;
        let row = &work.now[revision];
        if row.record.kind() != RecordKind::Revision
            || row.record.string("wiki_source_id") != Some(source_id.as_str())
        {
            return Err(corrupt("withdrawal inventory contains a foreign revision"));
        }
    }
    Ok(revisions)
}

fn emit_structural(
    work: &mut Work<'_>,
    structural: BTreeSet<RecordId>,
    delta: &mut CatalogDelta,
) -> Result<()> {
    for id in structural {
        let before: BTreeSet<_> = work
            .reader
            .outgoing_edges(&id, &[EligibilityRole::GenerationPacket])?
            .into_iter()
            .collect();
        let after: BTreeSet<_> = work.edge_overrides[&id]
            .iter()
            .filter(|edge| edge.role == EligibilityRole::GenerationPacket)
            .cloned()
            .collect();
        let facts = delta.facts.as_mut().unwrap();
        facts
            .edge_deletes
            .extend(before.difference(&after).cloned());
        facts
            .edge_inserts
            .extend(after.difference(&before).cloned());
        facts.records.push(RecordFactMutation {
            record_id: id.clone(),
            fact: work.facts[&id].clone(),
        });
        let row = &work.now[&id];
        let mut diagnostics = work
            .reader
            .diagnostics(&BTreeSet::from([row.path.clone()]))?;
        if let Some(old) = work.old.get(&id) {
            let prior = work
                .reader
                .eligibility_fact(&id)?
                .ok_or_else(|| corrupt("withdrawal structural fact missing"))?
                .structural
                .diagnostics(old);
            diagnostics.retain(|diagnostic| !prior.contains(diagnostic));
        }
        diagnostics.extend(work.facts[&id].structural.diagnostics(row));
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
    Ok(())
}

#[cfg(test)]
#[path = "withdraw_projection_tests.rs"]
mod tests;

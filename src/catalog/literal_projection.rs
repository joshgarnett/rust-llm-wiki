//! Sealed single-target ordinary-note publication, reusing indexed change recovery.
use super::{
    normalized_delta::{CatalogDelta, DocumentMutation, OwnedClaims, OwnedDiagnostics},
    query::QuerySnapshot,
    query_types::QueryCatalog,
    source_projection::{RefreshProjectionLimits, Work, deps},
    write_projection::{ProjectedWrite, ProjectedWriteParts, empty_delta, project_policy},
};
use crate::{
    changes::{
        ChangeDraft, ChangeEngine, ChangeManifest, ChangeStatus, ReadDependency,
        indexed_refresh::IndexedWriteOperation,
    },
    domain::{ErrorCode, Result, VaultRelativePath, WikiError},
    records::{ParsedNote, parse_note},
    vault::{ExpectedState, VaultFs},
};

/// Literal authoring is confined to visible ordinary Markdown. Knowledge,
/// retained machinery, generated navigation and every capture tree are excluded.
pub(crate) fn require_literal_path(path: &VaultRelativePath) -> Result<()> {
    let first = path.as_str().split('/').next().expect("nonempty path");
    let first = unicase::UniCase::unicode(first).to_folded_case();
    let last = path.as_str().rsplit('/').next().expect("nonempty path");
    if path
        .as_str()
        .split('/')
        .any(|component| component.starts_with('.'))
        || matches!(
            first.as_str(),
            ".wiki" | ".git" | ".obsidian" | "sources" | "changes" | "knowledge" | "runs"
        )
        || !path.as_str().ends_with(".md")
        || unicase::UniCase::unicode(last).to_folded_case() == "index.md"
        || unicase::UniCase::unicode(path.as_str()).to_folded_case() == "wiki.md"
    {
        return Err(WikiError::invalid(
            "literal edits require visible author Markdown outside managed, captured and generated paths",
        ));
    }
    Ok(())
}

/// Malformed author metadata is never interpreted as permission to strip it.
/// Even isolated declarations in an invalid envelope reserve the write boundary.
pub(crate) fn require_plain_bytes(bytes: &[u8]) -> Result<ParsedNote> {
    std::str::from_utf8(bytes).map_err(|_| WikiError::invalid(
        "literal edits require UTF-8 bytes; inspect the physical bytes and restore valid UTF-8 in an editor",
    ))?;
    let note = parse_note(bytes);
    if crate::sources::identity::has_wiki_claim(&note)? {
        let mut error = WikiError::invalid(
            "literal edit cannot adopt, strip or repair wiki metadata; valid Pages use guarded Page authoring",
        );
        error.hint = Some("Inspect exact bytes and preserve an outside-vault backup. Restore known valid metadata in an editor using trusted original bytes, then run index sync; missing trustworthy identity requires explicit new-record creation.".into());
        return Err(error);
    }
    Ok(note)
}
/// Called only after current byte splices have been reviewed. The sealed output
/// grants authority over one existing ordinary note and its derived bookkeeping.
pub(crate) fn project_literal(
    fs: &VaultFs,
    reader: &QuerySnapshot,
    mut draft: ChangeDraft,
    limits: &RefreshProjectionLimits,
) -> Result<Option<ProjectedWrite>> {
    if draft.origin.is_some() || !draft.allocated_ids.is_empty() || draft.operations.len() != 1 {
        return Err(WikiError::invalid(
            "literal admission requires one existing ordinary replacement without identity allocation or graph origin",
        ));
    }
    let op = &draft.operations[0];
    require_literal_path(&op.target)?;
    if !matches!(op.expected, ExpectedState::Hash(_)) || !op.apply_after.is_empty() {
        return Err(WikiError::invalid(
            "literal admission requires an existing hash-guarded unordered replacement",
        ));
    }
    let proposed = op
        .proposed
        .as_ref()
        .ok_or_else(|| WikiError::invalid("literal edit cannot delete files"))?;
    require_plain_bytes(proposed)?;
    if proposed.len() > crate::app::MAX_INPUT_BYTES {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "literal edit exceeds 16 MiB",
        ));
    }
    let mut work = Work::new(fs, reader, limits)?;
    work.capture(&op.target, &op.expected)?;
    let old = &work.captured[&op.target].bytes;
    require_plain_bytes(old)?;
    // A published typed owner at this path cannot be erased through a physical
    // plain-note edit, even after an external editor removed its metadata.
    if reader.document_metadata(&op.target)?.is_some_and(|row| {
        row.record_id.is_some() || row.source_id.is_some() || row.owner_revision.is_some()
    }) {
        return Err(WikiError::new(
            ErrorCode::FreshnessConflict,
            "literal target had canonical identity in the publication; inspect the removed metadata and reconcile with index sync",
        ));
    }
    if let Some(parent) = &draft.inverse_of {
        require_inverse(fs, parent, &draft)?;
    }
    if old == proposed {
        work.recheck()?;
        return Ok(None);
    }
    for dependency in &draft.read_preconditions {
        work.capture(&dependency.path, &dependency.expected)?;
    }
    let target = op.target.clone();
    work.charge(proposed.len())?;
    work.overlay.insert(target.clone(), proposed.clone());
    let policy = project_policy(&mut work)?;
    let mut delta = empty_delta(policy);
    let note = work.note(&target)?;
    delta.documents.push(DocumentMutation::Put {
        row: super::row_projection::canonical_document(&target, &note, None)?,
    });
    delta.claims.push(OwnedClaims {
        path: target.clone(),
        rows: vec![],
    });
    delta.diagnostics.push(OwnedDiagnostics {
        path: target.clone(),
        rows: diagnostics(&target, &note),
    });
    work.emit_links(&target, &mut delta)?;
    let mut after = work.before.clone();
    after.insert(target.clone(), ExpectedState::Hash(note.source_hash));
    draft.read_preconditions = deps(work.before.clone());
    delta.dependencies = deps(after.clone());
    delta.validate()?;
    work.recheck()?;
    Ok(Some(ProjectedWrite::from_parts(ProjectedWriteParts {
        operation: IndexedWriteOperation::LiteralEdit { path: target },
        draft,
        base: QueryCatalog::snapshot(reader).clone(),
        before: deps(work.before),
        after: deps(after),
        delta,
    })))
}

fn diagnostics(path: &VaultRelativePath, note: &ParsedNote) -> Vec<super::CatalogDiagnostic> {
    note.diagnostics
        .iter()
        .filter(|_| note.canonical.is_some())
        .map(|error| {
            super::scan::diagnostic(
                path,
                None,
                error.code,
                serde_json::json!({"message":error.message,"details":error.details}),
            )
        })
        .collect()
}

fn require_inverse(
    fs: &VaultFs,
    parent_id: &crate::domain::RecordId,
    draft: &ChangeDraft,
) -> Result<()> {
    let engine = ChangeEngine::new(fs.clone())?;
    let parent = engine.inspect_history(parent_id)?;
    if parent.status != ChangeStatus::Committed
        || parent.manifest.operations.len() != 1
        || parent.manifest.origin.is_some()
        || !parent.manifest.allocated_ids.is_empty()
    {
        return Err(WikiError::invalid(
            "literal inverse requires exact committed single-target ordinary history",
        ));
    }
    let op = &parent.manifest.operations[0];
    let child = &draft.operations[0];
    let before = engine
        .verify_payload(
            &parent.manifest.change_id,
            0,
            "before",
            &op.target,
            &op.before,
            &op.before_payload,
        )?
        .ok_or_else(|| WikiError::invalid("literal inverse parent before bytes are unavailable"))?;
    let after = engine
        .verify_payload(
            &parent.manifest.change_id,
            0,
            "proposed",
            &op.target,
            &op.after,
            &op.after_payload,
        )?
        .ok_or_else(|| WikiError::invalid("literal inverse parent after bytes are unavailable"))?;
    require_literal_path(&op.target)?;
    require_plain_bytes(&before)?;
    require_plain_bytes(&after)?;
    if child.target != op.target
        || child.expected != op.after
        || child.proposed.as_ref() != Some(&before)
        || op.role != crate::changes::OperationRole::MutableRecord
        || !op.apply_after.is_empty()
    {
        return Err(WikiError::invalid(
            "literal inverse differs from exact guarded parent reversal",
        ));
    }
    Ok(())
}

/// Replay validates exact retained payloads and constrains every row scope.
pub(crate) fn require_retained_literal(
    path: &VaultRelativePath,
    manifest: &ChangeManifest,
    engine: &ChangeEngine,
    delta: &CatalogDelta,
    before_guards: &[ReadDependency],
    after_guards: &[ReadDependency],
) -> Result<()> {
    require_literal_path(path)?;
    if manifest.operations.len() != 1
        || manifest.origin.is_some()
        || !manifest.allocated_ids.is_empty()
    {
        return Err(WikiError::invalid(
            "retained literal operation crosses its replacement boundary",
        ));
    }
    let op = &manifest.operations[0];
    if op.target != *path
        || op.role != crate::changes::OperationRole::MutableRecord
        || !op.apply_after.is_empty()
        || !matches!(op.before, ExpectedState::Hash(_))
        || !matches!(op.after, ExpectedState::Hash(_))
    {
        return Err(WikiError::invalid(
            "retained literal target, role or guards differ",
        ));
    }
    let before = engine
        .verify_payload(
            &manifest.change_id,
            0,
            "before",
            path,
            &op.before,
            &op.before_payload,
        )?
        .ok_or_else(|| WikiError::invalid("retained literal before bytes missing"))?;
    let after = engine
        .verify_payload(
            &manifest.change_id,
            0,
            "proposed",
            path,
            &op.after,
            &op.after_payload,
        )?
        .ok_or_else(|| WikiError::invalid("retained literal after bytes missing"))?;
    require_plain_bytes(&before)?;
    let note = require_plain_bytes(&after)?;
    if before_guards.len() != after_guards.len()
        || !before_guards.iter().zip(after_guards).all(|(old, new)| {
            old.path == new.path
                && if old.path == *path {
                    old.expected == op.before && new.expected == op.after
                } else {
                    old.expected == new.expected
                }
        })
        || !before_guards.iter().any(|guard| guard.path == *path)
    {
        return Err(WikiError::invalid(
            "literal replay supplemental guards mutate another path",
        ));
    }
    let expected = super::row_projection::canonical_document(path, &note, None)?;
    let facts = delta
        .facts
        .as_ref()
        .ok_or_else(|| WikiError::invalid("literal delta lacks normalized facts"))?;
    let policy = facts
        .policy
        .as_ref()
        .ok_or_else(|| WikiError::invalid("literal delta lacks policy membership"))?;
    if !delta.records.is_empty()
        || !delta.graph.is_empty()
        || !delta.revisions.is_empty()
        || !delta.owners.is_empty()
        || delta.documents != vec![DocumentMutation::Put { row: expected }]
        || delta.claims
            != vec![OwnedClaims {
                path: path.clone(),
                rows: vec![],
            }]
        || delta.diagnostics
            != vec![OwnedDiagnostics {
                path: path.clone(),
                rows: diagnostics(path, &note),
            }]
        || !facts.records.is_empty()
        || !facts.edge_inserts.is_empty()
        || !facts.edge_deletes.is_empty()
        || !facts.registry.is_empty()
        || delta.links.len() != 1
        || delta.links[0].path != *path
        || facts.links.len() != 1
        || facts.links[0].path != *path
        || facts.links[0]
            .rows
            .iter()
            .any(|fact| fact.from_path != *path || fact.typed.is_some())
        || !policy.retired_owners.is_empty()
        || !policy.replacements.is_empty()
        || policy.memberships.len() != 1
        || policy.memberships[0].path != *path
        || policy.memberships[0].hash != note.source_hash
        || !policy.memberships[0].keys.is_empty()
    {
        return Err(WikiError::invalid(
            "literal delta crosses its ordinary note and bookkeeping scope",
        ));
    }
    if let Some(parent) = &manifest.inverse_of {
        require_inverse(
            engine.fs(),
            parent,
            &ChangeDraft {
                title: manifest.title.clone(),
                origin: None,
                inverse_of: Some(parent.clone()),
                allocated_ids: Default::default(),
                read_preconditions: vec![],
                operations: vec![crate::changes::ExpectedWrite {
                    target: path.clone(),
                    expected: op.before.clone(),
                    proposed: Some(after),
                    apply_after: vec![],
                }],
            },
        )?;
    }
    Ok(())
}

/// Re-derive navigation against the pinned publication. Literal edits cannot
/// modify registry membership, so the base and intended registries are identical.
pub(super) fn require_literal_navigation(
    path: &VaultRelativePath,
    delta: &CatalogDelta,
    reader: &QuerySnapshot,
) -> Result<()> {
    let note = delta
        .documents
        .iter()
        .find_map(|row| match row {
            DocumentMutation::Put { row } if row.path == *path => {
                Some(parse_note(row.raw_text.as_bytes()))
            }
            _ => None,
        })
        .ok_or_else(|| WikiError::invalid("literal navigation lacks exact document"))?;
    let (links, facts) =
        literal_navigation(path, &note, &mut |key| reader.registry_probe_for_key(key))?;
    if delta.links
        != vec![super::normalized_delta::OwnedLinks {
            path: path.clone(),
            rows: links,
        }]
        || delta.facts.as_ref().is_none_or(|delta| {
            delta.links
                != vec![super::normalized_fact_delta::OwnedLinkFacts {
                    path: path.clone(),
                    rows: facts,
                }]
        })
    {
        return Err(WikiError::invalid(
            "retained literal navigation differs from exact body and published registry",
        ));
    }
    Ok(())
}
fn literal_navigation(
    path: &VaultRelativePath,
    note: &ParsedNote,
    probe: &mut impl FnMut(
        &super::link_facts::MatchKey,
    ) -> Result<super::navigation_resolution::RegistryProbe>,
) -> Result<(Vec<super::LinkRow>, Vec<super::link_facts::OwnedLinkFact>)> {
    use super::navigation_resolution::{self, NavigationResolution};
    let body = std::str::from_utf8(note.body())
        .map_err(|_| WikiError::invalid("literal navigation requires UTF-8"))?;
    let offset = note.raw.len() - note.body().len();
    let mut links = Vec::new();
    let mut facts = Vec::new();
    for link in crate::records::extract_links(body) {
        if links.len() >= super::normalized_delta::MAX_ROWS {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "literal navigation exceeds row allowance",
            ));
        }
        let resolution = navigation_resolution::resolve_untyped(&link.destination, probe)?;
        let fact = super::link_facts::untyped_navigation_fact(
            path,
            (offset + link.range.start) as u64,
            &link.destination,
            &resolution,
        )?;
        let (target_id, target_path) = match &resolution {
            NavigationResolution::Resolved { id, path, .. } => {
                (Some(id.clone()), Some(path.clone()))
            }
            _ => (None, None),
        };
        links.push(super::LinkRow {
            from_path: path.clone(),
            byte_start: fact.byte_start,
            target_id,
            target_path,
            resolution: format!("{resolution:?}"),
        });
        facts.push(fact);
    }
    links.sort_by_key(|row| row.byte_start);
    facts.sort_by_key(|row| row.byte_start);
    Ok((links, facts))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_delta_refuses_forged_claims_and_retained_navigation() {
        use crate::{
            catalog::Catalog,
            domain::{Blake3Hash, RecordId},
            vault::{VaultRoot, WriterPermit},
        };
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("WIKI.md"), b"---\nwiki_schema: '1'\nwiki_id: vault_literal_links\nwiki_kind: vault\ntitle: Links\n---\n").unwrap();
        let path = VaultRelativePath::new("ordinary.md").unwrap();
        let before = b"---\nauthor: me\n---\nBefore\n";
        std::fs::write(temp.path().join(path.as_str()), before).unwrap();
        let fs = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let writer = WriterPermit::acquire(fs.root(), std::time::Duration::ZERO).unwrap();
        let catalog = Catalog::new(fs.clone(), RecordId::new("vault_literal_links").unwrap());
        catalog.rebuild_normalized(&writer).unwrap();
        let reader = catalog.query_snapshot(Default::default()).unwrap();
        let projected = project_literal(
            &fs,
            &reader,
            ChangeDraft {
                title: "Literal links".into(),
                origin: None,
                inverse_of: None,
                allocated_ids: Default::default(),
                read_preconditions: vec![],
                operations: vec![crate::changes::ExpectedWrite {
                    target: path.clone(),
                    expected: ExpectedState::Hash(Blake3Hash::digest(before)),
                    proposed: Some(
                        b"---\nauthor: me\n---\nAfter [[WIKI]] and [[missing]].\n".to_vec(),
                    ),
                    apply_after: vec![],
                }],
            },
            &RefreshProjectionLimits::default(),
        )
        .unwrap()
        .unwrap()
        .into_parts();
        let delta = projected.delta;
        delta.validate().unwrap();
        require_literal_navigation(&path, &delta, &reader).unwrap();
        let mut forged = delta.clone();
        forged.links[0].rows[0].target_id = Some(RecordId::new("forged_target").unwrap());
        assert!(require_literal_navigation(&path, &forged, &reader).is_err());
        let mut forged = delta.clone();
        forged.facts.as_mut().unwrap().links[0].rows[0].raw_destination = "different".into();
        assert!(require_literal_navigation(&path, &forged, &reader).is_err());
        let mut forged = delta.clone();
        forged.links[0].rows.clear();
        forged.facts.as_mut().unwrap().links[0].rows.clear();
        assert!(require_literal_navigation(&path, &forged, &reader).is_err());
        let mut forged = delta.clone();
        forged.claims[0]
            .rows
            .push(super::super::types::IdentityClaimRow {
                id: RecordId::new("forged_claim").unwrap(),
                path: path.clone(),
                hash: Blake3Hash::digest(b"forged"),
                kind: None,
            });
        assert!(forged.validate().is_err());
        let mut forged = delta;
        if let DocumentMutation::Put { row } = &mut forged.documents[0] {
            row.kind = Some(crate::domain::RecordKind::Page);
        }
        assert!(forged.validate().is_err());
    }

    #[test]
    fn ordinary_properties_are_allowed_and_malformed_wiki_claims_refuse() {
        for bytes in [
            b"ordinary\r\n".as_slice(),
            b"---\nunknown: [a, b]\n---\nbody",
            b"\xef\xbb\xbf---\r\nauthor: yes # comment\r\n---\r\nbody",
        ] {
            assert!(require_plain_bytes(bytes).is_ok());
        }
        for bytes in [
            b"ordinary\xff\r\n".as_slice(),
            b"---\nwiki_id: [\n---\nbody".as_slice(),
            b"---\n'wiki_id': [\nbody",
            b"---\nwiki_kind: revision\n\xff",
            b"---\nwiki_id: a\nwiki_id: b\n---\nbody",
        ] {
            assert!(require_plain_bytes(bytes).is_err());
        }
    }

    #[test]
    fn path_boundary_excludes_captured_generated_and_machine_paths() {
        assert!(require_literal_path(&VaultRelativePath::new("notes/café.md").unwrap()).is_ok());
        for name in [
            "WIKI.md",
            "pages/index.md",
            "sources/s/revisions/r/content.md",
            "Sources/s/note.md",
            ".wiki/config.md",
            ".obsidian/settings.md",
            "knowledge/note.md",
            "runs/r/run.md",
            "changes/c/change.md",
            "notes/.wiki/private.md",
            "notes/.hidden.md",
        ] {
            assert!(
                require_literal_path(&VaultRelativePath::new(name).unwrap()).is_err(),
                "{name}"
            );
        }
    }
}

//! Bounded input collection shared by planning and deterministic admission replay.
use super::presentation::{
    self, AuthenticatedCapture, SourcePresentationInput, SourcePresentationPlan,
};
use crate::{
    changes::{ExpectedWrite, ReadDependency, ScanDocument},
    domain::{
        Blake3Hash, CanonicalRecord, ErrorCode, RecordId, RecordKind, Result, VaultRelativePath,
        WikiError, source_presentation::SourcePresentation,
    },
    records::{ParsedNote, parse_note},
    vault::ExpectedState,
};
use std::collections::{BTreeMap, BTreeSet};

type PresentationReader<'a> = dyn FnMut(&VaultRelativePath, usize) -> Result<Option<Vec<u8>>> + 'a;

pub(crate) struct PresentationChange {
    pub plan: SourcePresentationPlan,
    pub captured: Vec<ScanDocument>,
    pub dependencies: Vec<ReadDependency>,
}
pub(crate) struct NavigationDemand<'a> {
    pub full_copies: &'a BTreeSet<RecordId>,
    pub allocated_current: Option<&'a VaultRelativePath>,
    pub allocated_saved: &'a BTreeMap<RecordId, VaultRelativePath>,
}

pub(crate) fn proposed_source(
    note: &ParsedNote,
    changes: &BTreeMap<String, serde_json::Value>,
) -> Result<CanonicalRecord> {
    let mut fields = note
        .canonical
        .as_ref()
        .ok_or_else(|| WikiError::invalid("saved-note Source is invalid"))?
        .fields()
        .clone();
    fields.remove("wiki_presentation");
    fields.extend(changes.clone());
    CanonicalRecord::new(fields)
}

pub(crate) fn plan_change(
    source: &CanonicalRecord,
    source_body: &[u8],
    before: Option<&ParsedNote>,
    operations: &[ExpectedWrite],
    requested: &BTreeSet<RecordId>,
    read: &mut PresentationReader<'_>,
) -> Result<PresentationChange> {
    plan_change_for_navigation(
        source,
        source_body,
        before,
        operations,
        requested,
        NavigationDemand {
            full_copies: &BTreeSet::new(),
            allocated_current: None,
            allocated_saved: &BTreeMap::new(),
        },
        read,
    )
}

pub(crate) fn plan_change_for_navigation(
    source: &CanonicalRecord,
    source_body: &[u8],
    before: Option<&ParsedNote>,
    operations: &[ExpectedWrite],
    requested: &BTreeSet<RecordId>,
    demand: NavigationDemand<'_>,
    read: &mut PresentationReader<'_>,
) -> Result<PresentationChange> {
    let previous = before
        .and_then(|note| note.canonical.as_ref())
        .map(SourcePresentation::from_record)
        .transpose()?
        .flatten();
    if let (Some(previous), Some(before)) = (&previous, before) {
        previous.validate_owner(
            before
                .canonical
                .as_ref()
                .ok_or_else(|| WikiError::invalid("saved-note predecessor is invalid"))?,
        )?;
    }
    let current_path = match &previous {
        Some(previous) => previous.current.path.clone(),
        None => match demand.allocated_current {
            Some(path) => path.clone(),
            None => {
                presentation::allocate_path(source.title(), source.id(), None, &BTreeSet::new())?
            }
        },
    };
    let current_id = RecordId::new(
        source
            .string("wiki_current_revision")
            .ok_or_else(|| WikiError::invalid("Source head missing"))?,
    )?;
    let mut published = previous
        .as_ref()
        .map(|p| p.published.clone())
        .unwrap_or_default();
    for id in requested {
        published.entry(id.clone()).or_insert(None);
    }
    let mut captured = BTreeMap::new();
    let mut dependencies = BTreeMap::new();
    let mut remaining = 256 * 1024 * 1024usize;
    let mut observe = |path: &VaultRelativePath| -> Result<Option<Vec<u8>>> {
        if let Some(operation) = operations.iter().find(|operation| {
            &operation.target == path
                && !path
                    .as_str()
                    .starts_with(crate::domain::source_presentation::PRESENTATION_PREFIX)
        }) {
            return Ok(operation.proposed.clone());
        }
        if let Some(document) = captured.get(path) {
            let document: &ScanDocument = document;
            return Ok(Some(document.bytes.clone()));
        }
        let bytes = read(
            path,
            remaining.min(crate::changes::prepare::MAX_PAYLOAD_BYTES),
        )?;
        let state = match &bytes {
            None => ExpectedState::Absent,
            Some(bytes) => {
                remaining = remaining.checked_sub(bytes.len()).ok_or_else(|| {
                    WikiError::new(
                        ErrorCode::BudgetExceeded,
                        "saved-note inputs exceed 256 MiB",
                    )
                })?;
                let hash = Blake3Hash::digest(bytes);
                captured.insert(
                    path.clone(),
                    ScanDocument {
                        path: path.clone(),
                        hash: hash.clone(),
                        bytes: bytes.clone(),
                    },
                );
                ExpectedState::Hash(hash)
            }
        };
        dependencies.insert(path.clone(), state);
        Ok(bytes)
    };
    let ids: BTreeSet<_> = published
        .keys()
        .cloned()
        .chain(std::iter::once(current_id.clone()))
        .collect();
    let retained: BTreeSet<_> = source
        .field("wiki_revisions")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .collect();
    let mut records = BTreeMap::new();
    let mut contents = BTreeMap::new();
    let mut saved_paths = BTreeMap::new();
    for id in ids {
        if !retained.contains(id.as_str()) {
            return Err(WikiError::new(
                ErrorCode::SourceIntegrity,
                "published capture is not retained by Source",
            ));
        }
        let parent = format!("sources/{}/revisions/{id}", source.id());
        let path = VaultRelativePath::new(format!("{parent}/revision.md"))?;
        let bytes = observe(&path)?.ok_or_else(|| {
            WikiError::new(
                ErrorCode::SourceIntegrity,
                format!("saved-note revision {path} is missing"),
            )
        })?;
        let note = parse_note(&bytes);
        let revision = note.canonical.ok_or_else(|| {
            WikiError::new(
                ErrorCode::SourceIntegrity,
                "saved-note revision metadata is invalid",
            )
        })?;
        if revision.id() != &id
            || revision.kind() != RecordKind::Revision
            || revision.string("wiki_source_id") != Some(source.id().as_str())
        {
            return Err(WikiError::new(
                ErrorCode::SourceIntegrity,
                "saved-note revision has another identity or owner",
            ));
        }
        let materialize = published.get(&id).is_some_and(Option::is_none)
            && (id != current_id || demand.full_copies.contains(&id));
        if (id == current_id || materialize)
            && let Some(content) = revision.string("wiki_content_path")
        {
            let content_path = VaultRelativePath::new(format!("{parent}/{content}"))?;
            contents.insert(
                id.clone(),
                observe(&content_path)?.ok_or_else(|| {
                    WikiError::new(
                        ErrorCode::SourceIntegrity,
                        "saved-note capture content is missing",
                    )
                })?,
            );
        }
        if materialize {
            let date = revision
                .string("wiki_captured_at")
                .and_then(|date| date.get(..10))
                .ok_or_else(|| WikiError::invalid("saved-note date missing"))?;
            saved_paths.insert(
                id.clone(),
                match demand.allocated_saved.get(&id) {
                    Some(path) => path.clone(),
                    None => presentation::allocate_path(
                        source.title(),
                        &id,
                        Some(date),
                        &BTreeSet::new(),
                    )?,
                },
            );
        }
        records.insert(id, revision);
    }
    let mut observed = BTreeMap::new();
    observed.insert(current_path.clone(), observe(&current_path)?);
    for path in saved_paths.values() {
        observed.insert(path.clone(), observe(path)?);
    }
    for id in requested {
        if let Some(Some(saved)) = published.get(id) {
            observed.insert(saved.path.clone(), observe(&saved.path)?);
        }
    }
    let captures = records
        .iter()
        .map(|(id, revision)| {
            (
                id.clone(),
                AuthenticatedCapture {
                    revision,
                    content: contents.get(id).map(Vec::as_slice),
                },
            )
        })
        .collect();
    let plan = presentation::plan_source_presentation(SourcePresentationInput {
        source,
        source_body,
        previous: previous.as_ref(),
        captures: &captures,
        requested,
        allocated_current: &current_path,
        allocated_saved: &saved_paths,
        observed_views: &observed,
    })?;
    // Ownership metadata shares the existing bounded canonical envelope. Check
    // the exact assembled Source, including retained authored body and comments.
    let mut fields = source.fields().clone();
    fields.insert("wiki_presentation".into(), plan.metadata.encode()?.into());
    let assembled = if let Some(before) = before {
        let old = before
            .canonical
            .as_ref()
            .ok_or_else(|| WikiError::invalid("Source predecessor is invalid"))?;
        fields.retain(|key, value| old.field(key) != Some(value));
        crate::records::edit_note(before, &fields, Some(source_body), &before.source_hash)?
    } else {
        super::revision::record_bytes(CanonicalRecord::new(fields)?, source_body)?
    };
    if assembled.len() > crate::records::ParseLimits::default().max_envelope_bytes {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "Source metadata and authored notes exceed the existing 256 KiB envelope limit; move author notes to a Page before continuing",
        ));
    }
    Ok(PresentationChange {
        plan,
        captured: captured.into_values().collect(),
        dependencies: dependencies
            .into_iter()
            .map(|(path, expected)| ReadDependency { path, expected })
            .collect(),
    })
}

/// Admit only a replay of the deterministic renderer over authenticated before
/// images and immutable captures. Neither a path prefix nor output metadata
/// alone authorizes an extra write.
pub(crate) fn verify_change(
    before: Option<&ParsedNote>,
    after: &ParsedNote,
    operations: &[ExpectedWrite],
    requested: &BTreeSet<RecordId>,
    read: &mut PresentationReader<'_>,
) -> Result<BTreeSet<VaultRelativePath>> {
    let record = after
        .canonical
        .as_ref()
        .ok_or_else(|| WikiError::invalid("saved-note after-Source is invalid"))?;
    let Some(actual) = SourcePresentation::from_record(record)? else {
        if before
            .and_then(|note| note.canonical.as_ref())
            .is_some_and(|r| r.field("wiki_presentation").is_some())
        {
            return Err(WikiError::new(
                ErrorCode::SourceIntegrity,
                "Source presentation ownership cannot be removed",
            ));
        }
        return Ok(BTreeSet::new());
    };
    let mut fields = record.fields().clone();
    fields.remove("wiki_presentation");
    let source = CanonicalRecord::new(fields)?;
    let planned = plan_change(&source, after.body(), before, operations, requested, read)?;
    if planned.plan.metadata != actual {
        return Err(WikiError::new(
            ErrorCode::SourceIntegrity,
            "saved-note metadata differs from authenticated renderer replay",
        ));
    }
    let mut paths = BTreeSet::new();
    for expected in planned.plan.writes {
        let actual = operations
            .iter()
            .find(|operation| operation.target == expected.target)
            .ok_or_else(|| {
                WikiError::new(
                    ErrorCode::SourceIntegrity,
                    "Source update omits a required saved-note write",
                )
            })?;
        if actual.expected != expected.expected
            || actual.proposed != expected.proposed
            || actual.apply_after != expected.apply_after
        {
            return Err(WikiError::new(
                ErrorCode::SourceIntegrity,
                "saved-note write differs from authenticated renderer replay",
            ));
        }
        paths.insert(expected.target);
    }
    Ok(paths)
}

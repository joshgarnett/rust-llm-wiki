//! Source withdrawal and dependency invalidation plans.
use super::{capture::draft, revision::dependencies, types::*};
use crate::{
    changes::ExpectedWrite,
    domain::{RecordId, RecordKind, Result, WikiError},
    records::edit_note,
    vault::ExpectedState,
};
use std::collections::{BTreeMap, BTreeSet};
impl SourceView<'_> {
    pub(crate) fn invalidation(&self, source_id: &RecordId) -> Result<InvalidationInputs> {
        let (_, note) = self.resolve(source_id, RecordKind::Source, None)?;
        let source = note.canonical.as_ref().expect("resolved");
        let revision_ids = source
            .field("wiki_revisions")
            .and_then(serde_json::Value::as_array)
            .expect("validated revisions")
            .iter()
            .map(|v| RecordId::new(v.as_str().expect("validated ID")))
            .collect::<Result<Vec<_>>>()?;
        let mut assertion_ids = BTreeSet::new();
        for note in self.notes.values() {
            if let Some(record) = &note.canonical
                && record.kind() == RecordKind::Evidence
                && record.string("wiki_source_id") == Some(source_id.as_str())
            {
                assertion_ids.insert(RecordId::new(
                    record
                        .string("wiki_assertion_id")
                        .expect("validated assertion"),
                )?);
            }
        }
        Ok(InvalidationInputs {
            source_ids: vec![source_id.clone()],
            revision_ids,
            assertion_ids: assertion_ids.into_iter().collect(),
        })
    }
}
impl SourceStore {
    pub fn plan_withdraw(&self, source_id: &RecordId, reason: &str) -> Result<SourcePlan> {
        if reason.trim().is_empty() {
            return Err(WikiError::invalid("withdrawal requires a reason"));
        }
        let view = self.view()?;
        let (path, note) = view.resolve(source_id, RecordKind::Source, None)?;
        let source = note.canonical.as_ref().expect("resolved");
        let revision_id = RecordId::new(
            source
                .string("wiki_current_revision")
                .expect("validated head"),
        )?;
        let mut deps = BTreeMap::new();
        SourceView::note_dependency(path, note, &mut deps);
        let invalidation = view.invalidation(source_id)?;
        if source.string("wiki_status") == Some("withdrawn") {
            return Ok(SourcePlan {
                draft: None,
                source_id: source_id.clone(),
                revision_id,
                reused: true,
                invalidation,
                dependencies: dependencies(deps),
                capture_state: None,
            });
        }
        let changes = BTreeMap::from([
            ("wiki_status".into(), "withdrawn".into()),
            (
                "wiki_withdrawn_at".into(),
                super::revision::timestamp()?.into(),
            ),
            ("wiki_withdrawal_reason".into(), reason.into()),
        ]);
        let bytes = edit_note(note, &changes, None, &note.source_hash)?;
        let read_preconditions = dependencies(deps);
        let mut change = draft(
            format!("Withdraw {}", source.title()),
            vec![ExpectedWrite {
                target: path.clone(),
                expected: ExpectedState::Hash(note.source_hash.clone()),
                proposed: Some(bytes),
                apply_after: vec![],
            }],
            BTreeMap::new(),
        );
        change.read_preconditions = read_preconditions.clone();
        Ok(SourcePlan {
            draft: Some(change),
            source_id: source_id.clone(),
            revision_id,
            reused: false,
            invalidation,
            dependencies: read_preconditions,
            capture_state: None,
        })
    }
}

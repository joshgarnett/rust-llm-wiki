//! Exact-byte capture planning. Publication belongs to the changeset engine.
use super::{
    revision::{common, dependencies, integrity, record_bytes, timestamp},
    types::*,
};
use crate::{
    changes::{ChangeDraft, ExpectedWrite},
    domain::{
        Blake3Hash, CanonicalRecord, RecordId, RecordKind, Result, VaultRelativePath, WikiError,
    },
    records::{edit_note, parse_note},
    vault::ExpectedState,
};
use std::collections::BTreeMap;

pub(crate) struct Extraction {
    pub extractor: String,
    pub fingerprint: Blake3Hash,
    pub content: Option<Vec<u8>>,
}
impl Extraction {
    pub(super) fn capture_state(&self) -> SourceCaptureState {
        match self.content.as_deref() {
            None => SourceCaptureState::Unsupported,
            Some([]) => SourceCaptureState::Empty,
            Some(_) => SourceCaptureState::Complete,
        }
    }
}
pub(super) fn extract(request: &CaptureRequest) -> Result<Extraction> {
    let (extractor, fingerprint, content) = match &request.extraction {
        ExtractionInput::Utf8Preserve => (
            "utf8-preserve-v1".to_owned(),
            Blake3Hash::digest(b"utf8-preserve-v1"),
            std::str::from_utf8(&request.original)
                .ok()
                .map(|_| request.original.clone()),
        ),
        ExtractionInput::Unsupported {
            extractor,
            fingerprint,
        } => (extractor.clone(), fingerprint.clone(), None),
        ExtractionInput::Supplied {
            extractor,
            fingerprint,
            content,
        } => {
            std::str::from_utf8(content)
                .map_err(|_| WikiError::invalid("supplied extraction must be UTF-8"))?;
            (
                extractor.clone(),
                fingerprint.clone(),
                Some(content.clone()),
            )
        }
    };
    if extractor.is_empty() {
        return Err(WikiError::invalid("extractor must not be empty"));
    }
    Ok(Extraction {
        extractor,
        fingerprint,
        content,
    })
}
pub(crate) fn create_write(target: VaultRelativePath, bytes: Vec<u8>) -> ExpectedWrite {
    ExpectedWrite {
        target,
        expected: ExpectedState::Absent,
        proposed: Some(bytes),
        apply_after: vec![],
    }
}
pub(crate) fn draft(
    title: String,
    operations: Vec<ExpectedWrite>,
    allocated_ids: BTreeMap<String, RecordId>,
) -> ChangeDraft {
    ChangeDraft {
        title,
        origin: None,
        inverse_of: None,
        allocated_ids,
        operations,
        read_preconditions: vec![],
    }
}
pub(super) fn revision_writes(
    source_id: &RecordId,
    revision_id: &RecordId,
    source_path: &VaultRelativePath,
    request: &CaptureRequest,
    extraction: &Extraction,
) -> Result<Vec<ExpectedWrite>> {
    let parent = format!("sources/{source_id}/revisions/{revision_id}");
    let original_path = VaultRelativePath::new(format!("{parent}/original.bin"))?;
    let mut fields = common(revision_id, RecordKind::Revision, &request.title);
    for (key, value) in [
        ("wiki_source_id", source_id.as_str().to_owned()),
        ("wiki_source", format!("[[{source_path}]]")),
        ("wiki_captured_at", timestamp()?),
        ("wiki_original_path", "original.bin".into()),
        (
            "wiki_original_hash",
            Blake3Hash::digest(&request.original).to_string(),
        ),
        ("wiki_extractor", extraction.extractor.clone()),
        (
            "wiki_extractor_fingerprint",
            extraction.fingerprint.to_string(),
        ),
        (
            "wiki_extraction_status",
            if extraction.content.is_some() {
                "complete"
            } else {
                "unsupported"
            }
            .into(),
        ),
    ] {
        fields.insert(key.into(), value.into());
    }
    if let Some(media_type) = &request.media_type {
        fields.insert("wiki_media_type".into(), media_type.clone().into());
    }
    let mut operations = vec![create_write(original_path, request.original.clone())];
    if let Some(content) = &extraction.content {
        fields.insert("wiki_content_path".into(), "content.md".into());
        fields.insert(
            "wiki_content_hash".into(),
            Blake3Hash::digest(content).to_string().into(),
        );
        operations.push(create_write(
            VaultRelativePath::new(format!("{parent}/content.md"))?,
            content.clone(),
        ));
    }
    let mut manifest = create_write(
        VaultRelativePath::new(format!("{parent}/revision.md"))?,
        record_bytes(CanonicalRecord::new(fields)?, b"")?,
    );
    manifest.apply_after = operations.iter().map(|op| op.target.clone()).collect();
    operations.push(manifest);
    Ok(operations)
}
impl SourceStore {
    /// Capture an agent report with its optional, agent-claimed retrieval time.
    /// The timestamp is provenance on the immutable revision, separate from
    /// the local `wiki_captured_at` clock.
    pub fn plan_agent_capture(
        &self,
        request: CaptureRequest,
        retrieved_at: Option<&str>,
    ) -> Result<SourcePlan> {
        if request.origin_kind != SourceOrigin::AgentReport {
            return Err(WikiError::invalid(
                "agent capture requires agent-report origin",
            ));
        }
        if let Some(value) = retrieved_at {
            time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
                .map_err(|_| WikiError::invalid("retrieved_at must be RFC3339"))?;
        }
        let mut plan = self.plan_capture(request)?;
        if let Some(value) = retrieved_at {
            let revision = plan
                .draft
                .as_mut()
                .expect("capture has a draft")
                .operations
                .iter_mut()
                .find(|op| op.target.as_str().ends_with("/revision.md"))
                .expect("capture has a revision");
            let parsed = parse_note(revision.proposed.as_ref().expect("revision has bytes"));
            let mut fields = parsed
                .canonical
                .as_ref()
                .expect("generated revision is valid")
                .fields()
                .clone();
            fields.insert("origin_retrieved_at".into(), value.into());
            fields.insert("origin_retrieved_at_kind".into(), "agent-claimed".into());
            revision.proposed = Some(record_bytes(CanonicalRecord::new(fields)?, parsed.body())?);
        }
        Ok(plan)
    }

    pub fn plan_capture(&self, request: CaptureRequest) -> Result<SourcePlan> {
        let extraction = extract(&request)?;
        let source_id = RecordId::generate(RecordKind::Source)?;
        let revision_id = RecordId::generate(RecordKind::Revision)?;
        let source_path = VaultRelativePath::new(format!("sources/{source_id}/source.md"))?;
        let mut operations = revision_writes(
            &source_id,
            &revision_id,
            &source_path,
            &request,
            &extraction,
        )?;
        let mut fields = common(&source_id, RecordKind::Source, &request.title);
        fields.extend(BTreeMap::from([
            ("wiki_status".into(), "active".into()),
            (
                "wiki_origin_kind".into(),
                request.origin_kind.as_str().into(),
            ),
            ("wiki_origin".into(), request.origin.into()),
            ("wiki_current_revision".into(), revision_id.as_str().into()),
            (
                "wiki_revision".into(),
                format!("[[sources/{source_id}/revisions/{revision_id}/revision.md]]").into(),
            ),
            (
                "wiki_revisions".into(),
                serde_json::json!([revision_id.as_str()]),
            ),
        ]));
        let mut head = create_write(
            source_path,
            record_bytes(CanonicalRecord::new(fields)?, b"")?,
        );
        head.apply_after = operations.iter().map(|op| op.target.clone()).collect();
        operations.push(head);
        Ok(SourcePlan {
            draft: Some(draft(
                format!("Capture {}", request.title),
                operations,
                BTreeMap::from([
                    ("source".into(), source_id.clone()),
                    ("revision".into(), revision_id.clone()),
                ]),
            )),
            source_id,
            revision_id,
            reused: false,
            invalidation: InvalidationInputs::default(),
            dependencies: vec![],
            capture_state: Some(extraction.capture_state()),
        })
    }
    pub fn plan_refresh(
        &self,
        source_id: &RecordId,
        request: CaptureRequest,
    ) -> Result<SourcePlan> {
        self.plan_refresh_with_title(source_id, request, None)
    }

    /// Only an explicit title changes the canonical source display title.
    /// A file name supplied as the default capture title cannot rename a source.
    pub fn plan_refresh_with_title(
        &self,
        source_id: &RecordId,
        mut request: CaptureRequest,
        explicit_title: Option<&str>,
    ) -> Result<SourcePlan> {
        let extraction = extract(&request)?;
        let view = self.view()?;
        let (source_path, source_note) = view.resolve(source_id, RecordKind::Source, None)?;
        let source = source_note.canonical.as_ref().expect("resolved");
        let title = match explicit_title {
            Some(title) if title.trim().is_empty() => {
                return Err(WikiError::invalid("source title must not be empty"));
            }
            Some(title) => title,
            None => source.title(),
        };
        request.title = title.to_owned();
        let title_changed = title != source.title();
        let old_head = RecordId::new(
            source
                .string("wiki_current_revision")
                .expect("validated head"),
        )?;
        let mut deps = BTreeMap::new();
        SourceView::note_dependency(source_path, source_note, &mut deps);
        let retained = source
            .field("wiki_revisions")
            .and_then(serde_json::Value::as_array)
            .expect("validated revisions");
        let ids: std::collections::BTreeSet<_> = retained
            .iter()
            .map(|v| v.as_str().expect("validated ID"))
            .collect();
        if ids.len() != retained.len() || !ids.contains(old_head.as_str()) {
            return Err(integrity(
                "source has duplicate revisions or an unretained head",
            ));
        }
        view.resolve(
            &old_head,
            RecordKind::Revision,
            source.string("wiki_revision"),
        )?;
        let mut reused = None;
        for revision_id in retained {
            let revision_id = RecordId::new(revision_id.as_str().expect("validated ID"))?;
            let (path, note) = view.resolve(&revision_id, RecordKind::Revision, None)?;
            SourceView::note_dependency(path, note, &mut deps);
            let record = note.canonical.as_ref().expect("resolved");
            if record.string("wiki_source_id") != Some(source_id.as_str()) {
                return Err(integrity("retained revision ownership mismatch"));
            }
            view.resolve(source_id, RecordKind::Source, record.string("wiki_source"))?;
            let parent = path
                .as_str()
                .rsplit_once('/')
                .ok_or_else(|| integrity("revision has no parent"))?
                .0;
            let original = view.read(
                &VaultRelativePath::new(format!(
                    "{parent}/{}",
                    record.string("wiki_original_path").expect("validated path")
                ))?,
                &mut deps,
            )?;
            if Blake3Hash::digest(&original).as_str()
                != record.string("wiki_original_hash").expect("validated hash")
            {
                return Err(integrity("original hash mismatch during refresh"));
            }
            let content = if record.string("wiki_extraction_status") == Some("complete") {
                let bytes = view.read(
                    &VaultRelativePath::new(format!(
                        "{parent}/{}",
                        record.string("wiki_content_path").expect("complete path")
                    ))?,
                    &mut deps,
                )?;
                if Blake3Hash::digest(&bytes).as_str()
                    != record.string("wiki_content_hash").expect("complete hash")
                {
                    return Err(integrity("content hash mismatch during refresh"));
                }
                Some(bytes)
            } else {
                None
            };
            if original == request.original
                && content == extraction.content
                && record.string("wiki_extractor_fingerprint")
                    == Some(extraction.fingerprint.as_str())
            {
                if revision_id == old_head {
                    reused = Some(revision_id);
                    break;
                }
                reused.get_or_insert(revision_id);
            }
        }
        let is_reused = reused.is_some();
        let revision_id = match reused {
            Some(id) => id,
            None => RecordId::generate(RecordKind::Revision)?,
        };
        if is_reused && revision_id == old_head && !title_changed {
            return Ok(SourcePlan {
                draft: None,
                source_id: source_id.clone(),
                revision_id,
                reused: true,
                invalidation: InvalidationInputs::default(),
                dependencies: dependencies(deps),
                capture_state: Some(extraction.capture_state()),
            });
        }
        let mut operations = if is_reused {
            vec![]
        } else {
            revision_writes(source_id, &revision_id, source_path, &request, &extraction)?
        };
        let mut revisions = retained.clone();
        if !is_reused {
            revisions.push(revision_id.as_str().into());
        }
        let revision_path = if is_reused {
            view.resolve(&revision_id, RecordKind::Revision, None)?
                .0
                .clone()
        } else {
            VaultRelativePath::new(format!(
                "sources/{source_id}/revisions/{revision_id}/revision.md"
            ))?
        };
        let mut changes = BTreeMap::from([
            ("wiki_current_revision".into(), revision_id.as_str().into()),
            (
                "wiki_revision".into(),
                format!("[[{revision_path}]]").into(),
            ),
            ("wiki_revisions".into(), serde_json::Value::Array(revisions)),
        ]);
        if title_changed {
            changes.insert("title".into(), title.into());
        }
        let head = ExpectedWrite {
            target: source_path.clone(),
            expected: ExpectedState::Hash(source_note.source_hash.clone()),
            proposed: Some(edit_note(
                source_note,
                &changes,
                None,
                &source_note.source_hash,
            )?),
            apply_after: operations.iter().map(|op| op.target.clone()).collect(),
        };
        operations.push(head);
        let invalidation = view.invalidation(source_id)?;
        let read_preconditions = dependencies(deps);
        let mut change = draft(
            format!("Refresh {}", source.title()),
            operations,
            if is_reused {
                BTreeMap::new()
            } else {
                BTreeMap::from([("revision".into(), revision_id.clone())])
            },
        );
        change.read_preconditions = read_preconditions.clone();
        Ok(SourcePlan {
            draft: Some(change),
            source_id: source_id.clone(),
            revision_id,
            reused: is_reused,
            invalidation,
            dependencies: read_preconditions,
            capture_state: Some(extraction.capture_state()),
        })
    }
}

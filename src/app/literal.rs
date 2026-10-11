//! Cache-free physical inspection and byte-preserving guarded note edits.
use super::{MAX_INPUT_BYTES, MutationOutcome, OfflineApp};
use crate::{
    changes::{ChangeDraft, ChangeStatus, ExpectedWrite, PreparedChange},
    domain::{Blake3Hash, ByteSpan, ErrorCode, RecordKind, Result, VaultRelativePath, WikiError},
    records::{ParseStatus, parse_note},
    vault::{ExpectedState, VaultFs, fs::read_regular_bounded},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct LiteralInspectRequest {
    pub path: VaultRelativePath,
    pub range: Option<ByteSpan>,
    pub max_bytes: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct LiteralInspection {
    pub version: &'static str,
    pub state_scope: &'static str,
    pub coordinates: &'static str,
    pub structured_currentness_asserted: bool,
    pub path: VaultRelativePath,
    pub hash: Blake3Hash,
    pub file_bytes: u64,
    pub range: ByteSpan,
    pub bytes: Vec<u8>,
    pub text: Option<String>,
    pub parse_status: &'static str,
    pub diagnostics: Vec<WikiError>,
    pub envelope_content_range: Option<ByteSpan>,
    pub body_range: Option<ByteSpan>,
    pub truncated: bool,
    pub continuation: Option<ByteSpan>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiteralByteEdit {
    pub range: ByteSpan,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement: Option<Vec<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement_utf8: Option<String>,
}
impl LiteralByteEdit {
    fn replacement_bytes(&self) -> Result<&[u8]> {
        match (&self.replacement, &self.replacement_utf8) {
            (Some(bytes), None) => Ok(bytes),
            (None, Some(text)) => Ok(text.as_bytes()),
            _ => Err(usage(
                "each edit requires exactly one replacement or replacement_utf8",
            )),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiteralEditPatch {
    pub version: String,
    pub edits: Vec<LiteralByteEdit>,
}

#[derive(Debug, Clone)]
pub struct LiteralEditRequest {
    pub path: VaultRelativePath,
    pub if_match: Blake3Hash,
    pub patch: LiteralEditPatch,
}

/// Physical regular-file bytes only. This does not need a valid WIKI envelope,
/// an idle operation ledger, a cache, or provider configuration.
pub fn inspect_literal(fs: &VaultFs, request: LiteralInspectRequest) -> Result<LiteralInspection> {
    if request.max_bytes == 0 || request.max_bytes > MAX_INPUT_BYTES {
        return Err(usage("literal inspection byte limit must be 1..=16 MiB"));
    }
    let bytes = literal_bytes(fs, &request.path)?;
    let wanted = request
        .range
        .unwrap_or(ByteSpan::new(0, bytes.len() as u64)?);
    if wanted.end() > bytes.len() as u64 {
        return Err(usage(
            "literal inspection range exceeds physical file bytes",
        ));
    }
    let start = usize::try_from(wanted.start()).map_err(|_| usage("range exceeds host size"))?;
    let wanted_end = usize::try_from(wanted.end()).map_err(|_| usage("range exceeds host size"))?;
    let end = wanted_end.min(start.saturating_add(request.max_bytes));
    let note = parse_note(&bytes);
    let certain_boundary = note.fields.is_some() || note.status == ParseStatus::NoFrontmatter;
    let envelope_content_range = if certain_boundary {
        note.envelope
            .as_ref()
            .map(|range| ByteSpan::new(range.start as u64, range.end as u64))
            .transpose()?
    } else {
        None
    };
    let body_range = certain_boundary
        .then(|| ByteSpan::new(note.body_start as u64, bytes.len() as u64))
        .transpose()?;
    let returned = bytes[start..end].to_vec();
    Ok(LiteralInspection {
        version: "lwiki.literal-inspection.v1",
        state_scope: "physical_bytes_uncommitted_state_possible",
        coordinates: "whole_file_bytes",
        structured_currentness_asserted: false,
        path: request.path,
        hash: note.source_hash,
        file_bytes: bytes.len() as u64,
        text: std::str::from_utf8(&returned).ok().map(str::to_owned),
        bytes: returned,
        range: ByteSpan::new(start as u64, end as u64)?,
        parse_status: match note.status {
            ParseStatus::NoFrontmatter => "no_frontmatter",
            ParseStatus::Valid => "valid",
            ParseStatus::Invalid => "invalid",
            ParseStatus::UnsupportedSchema => "unsupported_schema",
        },
        diagnostics: note.diagnostics,
        envelope_content_range,
        body_range,
        truncated: end < wanted_end,
        continuation: (end < wanted_end)
            .then(|| ByteSpan::new(end as u64, wanted_end as u64))
            .transpose()?,
    })
}

impl OfflineApp {
    pub fn inspect_literal(&self, request: LiteralInspectRequest) -> Result<LiteralInspection> {
        inspect_literal(&self.fs, request)
    }

    /// Splices use reviewed whole-file coordinates, including frontmatter/BOM.
    pub fn edit_literal(&self, request: LiteralEditRequest) -> Result<MutationOutcome> {
        crate::catalog::literal_projection::require_literal_path(&request.path)?;
        let old = literal_bytes(&self.fs, &request.path)?;
        if Blake3Hash::digest(&old) != request.if_match {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "literal edit author hash differs from current bytes",
            ));
        }
        let proposed = splice_bytes(&old, &request.patch)?;
        let before = parse_note(&old);
        if before
            .canonical
            .as_ref()
            .is_some_and(|r| r.kind() == RecordKind::Page)
        {
            return self.page_put(request.path, proposed, Some(request.if_match));
        }
        crate::catalog::literal_projection::require_plain_bytes(&old)?;
        crate::catalog::literal_projection::require_plain_bytes(&proposed)?;
        self.execute_literal_draft(
            ChangeDraft {
                title: format!("Edit {}", request.path),
                origin: None,
                inverse_of: None,
                allocated_ids: BTreeMap::new(),
                read_preconditions: vec![],
                operations: vec![ExpectedWrite {
                    target: request.path,
                    expected: ExpectedState::Hash(request.if_match),
                    proposed: Some(proposed),
                    apply_after: vec![],
                }],
            },
            false,
        )
    }

    pub(crate) fn execute_literal_draft(
        &self,
        draft: ChangeDraft,
        force_stage: bool,
    ) -> Result<MutationOutcome> {
        use crate::catalog::{
            literal_projection::project_literal, query_types::QueryReadLimits,
            source_projection::RefreshProjectionLimits, source_refresh::IndexedRefreshSession,
        };
        let catalog = self.catalog();
        if catalog.operation_state()?.is_none() {
            return self.execute_draft(draft, force_stage);
        }
        let mut outcome = MutationOutcome {
            source_capture: None,
            plan: super::offline::summarize(
                &draft.title,
                &draft.read_preconditions,
                &draft.operations,
            ),
            allocated_ids: BTreeMap::new(),
            change: None,
            status: None,
            snapshot: None,
            reused: false,
        };
        if self.options.dry_run {
            // Byte guards have been checked above; indexed admission is unresolved.
            return Ok(outcome);
        }
        catalog.guard_query()?;
        let reader = catalog.query_snapshot(QueryReadLimits::default())?;
        let projected = project_literal(
            &self.fs,
            &reader,
            draft,
            &RefreshProjectionLimits::default(),
        )?;
        let Some(projected) = projected else {
            outcome.plan.operations.clear();
            outcome.reused = true;
            return Ok(outcome);
        };
        drop(reader);
        let draft = projected.draft();
        outcome.plan =
            super::offline::summarize(&draft.title, &draft.read_preconditions, &draft.operations);
        let writer = self.writer()?;
        catalog.guard_current(None)?;
        let mut session = IndexedRefreshSession::prepare_write(&catalog, &writer, projected)?;
        let change = session.proof().change.clone();
        outcome.change = Some(change.clone());
        if force_stage || self.options.stage_only {
            outcome.status = Some(ChangeStatus::Prepared);
            return Ok(outcome);
        }
        let report = self
            .engine()?
            .apply_indexed_refresh(&writer, &mut session)
            .map_err(|error| super::offline::retained_error(error, &change))?;
        outcome.status = Some(report.status);
        outcome.snapshot = report.snapshot;
        Ok(outcome)
    }

    /// Rollback retains the exact parent reversal and stages it for review.
    pub(crate) fn literal_rollback(&self, change: &PreparedChange) -> Result<MutationOutcome> {
        let draft = self.engine()?.inverse_plan(change)?.draft;
        self.execute_literal_draft(draft, true)
    }
}

fn literal_bytes(fs: &VaultFs, path: &VaultRelativePath) -> Result<Vec<u8>> {
    if path
        .as_str()
        .split('/')
        .any(|component| component.starts_with('.'))
    {
        return Err(usage(
            "literal inspection accepts visible files; hidden wiki, editor and repository state require dedicated diagnostics",
        ));
    }
    read_regular_bounded(fs.root(), path, true, MAX_INPUT_BYTES, "literal-inspection")?.ok_or_else(
        || {
            WikiError::new(
                ErrorCode::RecordNotFound,
                format!("literal path is absent: {path}"),
            )
        },
    )
}

fn usage(message: &str) -> WikiError {
    WikiError::new(ErrorCode::Usage, message)
}

fn splice_bytes(before: &[u8], patch: &LiteralEditPatch) -> Result<Vec<u8>> {
    if patch.version != "lwiki.literal-edit.v1" || patch.edits.is_empty() || patch.edits.len() > 128
    {
        return Err(usage(
            "literal edit requires lwiki.literal-edit.v1 and 1–128 ordered edits",
        ));
    }
    let mut length = before.len();
    let mut previous_end = 0;
    let mut previous_start = None;
    for edit in &patch.edits {
        let start = usize::try_from(edit.range.start())
            .map_err(|_| usage("edit range exceeds host size"))?;
        let end =
            usize::try_from(edit.range.end()).map_err(|_| usage("edit range exceeds host size"))?;
        if end > before.len() || start < previous_end || previous_start == Some(start) {
            return Err(usage(
                "literal edits must be ordered, nonoverlapping, distinct and within the original file",
            ));
        }
        length = length
            .checked_sub(end - start)
            .and_then(|length| length.checked_add(edit.replacement_bytes().ok()?.len()))
            .ok_or_else(|| usage("literal edit replacement or size is invalid"))?;
        if length > MAX_INPUT_BYTES {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "literal edit exceeds 16 MiB",
            ));
        }
        previous_start = Some(start);
        previous_end = end;
    }
    let mut after = Vec::new();
    after
        .try_reserve_exact(length)
        .map_err(|_| WikiError::new(ErrorCode::BudgetExceeded, "literal edit allocation failed"))?;
    let mut cursor = 0;
    for edit in &patch.edits {
        let start = edit.range.start() as usize;
        let end = edit.range.end() as usize;
        after.extend_from_slice(&before[cursor..start]);
        after.extend_from_slice(edit.replacement_bytes()?);
        cursor = end;
    }
    after.extend_from_slice(&before[cursor..]);
    Ok(after)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::VaultRoot;
    use std::fs;

    fn patch(range: ByteSpan, text: &str) -> LiteralEditPatch {
        LiteralEditPatch {
            version: "lwiki.literal-edit.v1".into(),
            edits: vec![LiteralByteEdit {
                range,
                replacement: None,
                replacement_utf8: Some(text.into()),
            }],
        }
    }

    #[test]
    fn byte_splices_preserve_author_envelope_unicode_bom_crlf_and_arbitrary_bytes() {
        let before = b"\xef\xbb\xbf---\r\nunknown: yes # comment\r\n---\r\nold \xff\r\n";
        let start = before.windows(3).position(|b| b == b"old").unwrap();
        let after = splice_bytes(
            before,
            &patch(
                ByteSpan::new(start as u64, start as u64 + 3).unwrap(),
                "東京",
            ),
        )
        .unwrap();
        assert_eq!(&after[..start], &before[..start]);
        assert_eq!(&after[start + "東京".len()..], &before[start + 3..]);
    }

    #[test]
    fn strict_patch_contract_rejects_overlap_and_ambiguous_replacement() {
        let mut value = patch(ByteSpan::new(1, 3).unwrap(), "x");
        value.edits.push(LiteralByteEdit {
            range: ByteSpan::new(2, 4).unwrap(),
            replacement: Some(vec![]),
            replacement_utf8: None,
        });
        assert!(splice_bytes(b"hello", &value).is_err());
        value.edits.truncate(1);
        value.edits[0].replacement = Some(vec![]);
        assert!(splice_bytes(b"hello", &value).is_err());
        assert!(
            serde_json::from_str::<LiteralEditPatch>(
                r#"{"version":"lwiki.literal-edit.v1","edits":[],"guess":true}"#
            )
            .is_err()
        );
    }

    #[test]
    fn physical_inspection_works_without_marker_cache_or_utf8_and_never_guesses_body() {
        let temp = tempfile::tempdir().unwrap();
        let fs = VaultFs::new(VaultRoot::for_initialization(temp.path()).unwrap());
        let path = VaultRelativePath::new("damaged.md").unwrap();
        let bytes = b"---\nwiki_id: damaged\ninvalid: [\nbody? \xff";
        std::fs::write(temp.path().join(path.as_str()), bytes).unwrap();
        let outcome = inspect_literal(
            &fs,
            LiteralInspectRequest {
                path,
                range: None,
                max_bytes: 7,
            },
        )
        .unwrap();
        assert_eq!(outcome.hash, Blake3Hash::digest(bytes));
        assert_eq!(outcome.bytes, &bytes[..7]);
        assert_eq!(outcome.body_range, None);
        assert_eq!(outcome.envelope_content_range, None);
        assert!(outcome.truncated);
        assert!(!outcome.structured_currentness_asserted);
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
    }

    #[test]
    fn suspected_or_incomplete_opener_inspection_keeps_bytes_and_unknown_coordinates() {
        let temp = tempfile::tempdir().unwrap();
        let fs = VaultFs::new(VaultRoot::for_initialization(temp.path()).unwrap());
        let path = VaultRelativePath::new("damaged-opener.md").unwrap();
        let mut oversized = b"author: ".to_vec();
        oversized.resize(crate::sources::identity::MAX_RESERVATION_BYTES + 1, b'x');
        for bytes in [b"\xef\xbb\xbf--\r\nwiki_schema: '1'\r\nwiki_kind: page\r\nwiki_id: damaged\r\n---\r\nBody\r\n".to_vec(), oversized] {
            std::fs::write(temp.path().join(path.as_str()), &bytes).unwrap();
            let inspected = inspect_literal(&fs, LiteralInspectRequest { path: path.clone(), range: None, max_bytes: MAX_INPUT_BYTES }).unwrap();
            assert_eq!(inspected.bytes, bytes);
            assert_eq!(inspected.hash, Blake3Hash::digest(&bytes));
            assert_eq!(inspected.parse_status, "invalid");
            assert!(!inspected.diagnostics.is_empty());
            assert!(inspected.envelope_content_range.is_none());
            assert!(inspected.body_range.is_none());
            assert!(!inspected.structured_currentness_asserted);
            assert!(crate::catalog::literal_projection::require_plain_bytes(&bytes).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn physical_inspection_refuses_symlinks_and_out_of_range() {
        let temp = tempfile::tempdir().unwrap();
        let fs = VaultFs::new(VaultRoot::for_initialization(temp.path()).unwrap());
        fs::write(temp.path().join("note.md"), b"note").unwrap();
        std::os::unix::fs::symlink("note.md", temp.path().join("link.md")).unwrap();
        assert!(
            inspect_literal(
                &fs,
                LiteralInspectRequest {
                    path: VaultRelativePath::new("link.md").unwrap(),
                    range: None,
                    max_bytes: 8
                }
            )
            .is_err()
        );
        assert!(
            inspect_literal(
                &fs,
                LiteralInspectRequest {
                    path: VaultRelativePath::new("note.md").unwrap(),
                    range: Some(ByteSpan::new(0, 5).unwrap()),
                    max_bytes: 8
                }
            )
            .is_err()
        );
    }
}

#[cfg(test)]
mod inspection_boundary_tests {
    use super::*;
    use crate::vault::VaultRoot;

    #[test]
    fn physical_inspection_refuses_hidden_state_at_every_depth() {
        let temp = tempfile::tempdir().unwrap();
        let fs = VaultFs::new(VaultRoot::for_initialization(temp.path()).unwrap());
        for path in [
            ".wiki/provider-config.toml",
            ".git/config",
            ".obsidian/settings.json",
            "notes/.wiki/accounting.md",
            "notes/.Git/config",
            "notes/.hidden.md",
        ] {
            assert!(
                inspect_literal(
                    &fs,
                    LiteralInspectRequest {
                        path: VaultRelativePath::new(path).unwrap(),
                        range: None,
                        max_bytes: 1024
                    }
                )
                .is_err(),
                "{path}"
            );
        }
    }
}

#[cfg(test)]
mod workflow_tests {
    use super::*;
    use crate::catalog::query_types::QueryCatalog;
    use crate::{
        app::{OperationOptions, offline::init},
        vault::VaultRoot,
    };
    use std::fs;

    fn fixture(normalized: bool, migrated: bool) -> (tempfile::TempDir, OfflineApp) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("literal vault café");
        let options = OperationOptions {
            offline: true,
            lock_timeout_ms: 200,
            ..Default::default()
        };
        init(&root, "Literal workflow", options).unwrap();
        let vault = VaultFs::new(VaultRoot::explicit(&root).unwrap());
        if migrated {
            let writer =
                crate::vault::WriterPermit::acquire(vault.root(), std::time::Duration::ZERO)
                    .unwrap();
            crate::storage::cleanup(&vault, &writer, &crate::storage::StorageOptions::default())
                .unwrap();
        }
        fs::create_dir(root.join("notes")).unwrap();
        fs::write(root.join("notes/ordinary.md"), "\u{feff}---\r\nauthor: café # keep my comment\r\ncustom: [one, two]\r\n---\r\n# Guide\r\nCapacity is amber.\r\n\r\n```lwiki-graph-review-v1\r\nnot JSON\r\n```\r\n").unwrap();
        let app = OfflineApp::new(vault, options).unwrap();
        if normalized {
            app.index_rebuild_normalized().unwrap();
        } else {
            app.index_sync(false).unwrap();
        }
        (temp, app)
    }
    fn patch(range: ByteSpan, text: &str) -> LiteralEditPatch {
        LiteralEditPatch {
            version: "lwiki.literal-edit.v1".into(),
            edits: vec![LiteralByteEdit {
                range,
                replacement: None,
                replacement_utf8: Some(text.into()),
            }],
        }
    }
    fn replace_word(path: &str, before: &[u8], from: &[u8], to: &str) -> LiteralEditRequest {
        let start = before
            .windows(from.len())
            .position(|bytes| bytes == from)
            .unwrap();
        LiteralEditRequest {
            path: VaultRelativePath::new(path).unwrap(),
            if_match: Blake3Hash::digest(before),
            patch: patch(
                ByteSpan::new(start as u64, (start + from.len()) as u64).unwrap(),
                to,
            ),
        }
    }

    #[test]
    fn ordinary_note_guarded_stage_apply_retry_rollback_and_rebuild_both_layouts() {
        for (normalized, migrated) in [(false, false), (true, false), (true, true)] {
            let (_temp, app) = fixture(normalized, migrated);
            let path = app.fs.root().path().join("notes/ordinary.md");
            let before = fs::read(&path).unwrap();
            let request = replace_word("notes/ordinary.md", &before, b"amber", "violet 東京");
            let expected = splice_bytes(&before, &request.patch).unwrap();
            let staged_app = OfflineApp::new(
                app.fs.clone(),
                OperationOptions {
                    stage_only: true,
                    ..app.options
                },
            )
            .unwrap();
            let staged = staged_app.edit_literal(request.clone()).unwrap();
            assert_eq!(staged.status, Some(ChangeStatus::Prepared));
            assert_eq!(fs::read(&path).unwrap(), before);
            let retained = staged.change.unwrap();
            let payload = app.changes_payload(retained.change_id.clone(), 0).unwrap();
            assert_eq!(payload.before.as_deref(), Some(before.as_slice()));
            assert_eq!(payload.proposed.as_deref(), Some(expected.as_slice()));
            let applied = app.changes_apply(retained.change_id.clone()).unwrap();
            assert_eq!(applied.status, Some(ChangeStatus::Committed));
            assert_eq!(fs::read(&path).unwrap(), expected);
            assert!(
                app.changes_apply(retained.change_id.clone())
                    .unwrap()
                    .reused
            );
            let stale = app.edit_literal(request).unwrap_err();
            assert_eq!(stale.code, ErrorCode::ContentConflict);
            assert_eq!(fs::read(&path).unwrap(), expected);
            let inverse = app.changes_rollback(retained.change_id).unwrap();
            assert_eq!(inverse.status, Some(ChangeStatus::Prepared));
            assert_eq!(fs::read(&path).unwrap(), expected);
            app.changes_apply(inverse.change.unwrap().change_id)
                .unwrap();
            assert_eq!(fs::read(&path).unwrap(), before);
            if normalized {
                app.index_rebuild_normalized().unwrap();
            } else {
                app.index_sync(true).unwrap();
            }
            let inspected = app
                .inspect_literal(LiteralInspectRequest {
                    path: VaultRelativePath::new("notes/ordinary.md").unwrap(),
                    range: None,
                    max_bytes: MAX_INPUT_BYTES,
                })
                .unwrap();
            assert_eq!(inspected.bytes, before);
            assert!(!inspected.structured_currentness_asserted);
        }
    }

    #[test]
    fn incomplete_identity_observation_preserves_publication_and_literal_inspection() {
        for (normalized, migrated) in [(false, false), (true, false), (true, true)] {
            let (_temp, app) = fixture(normalized, migrated);
            let health = app.catalog().published_health(Default::default()).unwrap();
            assert_eq!(health.diagnostic_count, 0);
            let before = app
                .catalog()
                .cached_query_snapshot(Default::default())
                .unwrap();
            let snapshot = crate::catalog::query_types::QueryCatalog::snapshot(&before).clone();
            // Cleanup adds a visible ordinary guide only in the migrated layout.
            // Init's landing prose remains inside canonical WIKI.md.
            let expected_paths = if migrated {
                vec!["STORAGE.md", "notes/ordinary.md"]
            } else {
                vec!["notes/ordinary.md"]
            };
            let inspection_paths = {
                let sql = if normalized {
                    "SELECT path FROM documents WHERE record_id IS NULL AND source_id IS NULL AND owner_revision IS NULL AND eligibility='unsupported' ORDER BY path"
                } else {
                    "SELECT path FROM documents WHERE gen=?1 AND record_id IS NULL AND source_id IS NULL AND owner_revision IS NULL AND eligibility='unsupported' ORDER BY path"
                };
                let mut statement = before.connection().prepare(sql).unwrap();
                let mut rows = if normalized {
                    statement.query([]).unwrap()
                } else {
                    statement
                        .query([i64::try_from(snapshot.generation).unwrap()])
                        .unwrap()
                };
                let mut paths = Vec::new();
                while let Some(row) = rows.next().unwrap() {
                    paths.push(row.get::<_, String>(0).unwrap());
                }
                paths
            };
            assert_eq!(
                inspection_paths, expected_paths,
                "normalized={normalized}, migrated={migrated}"
            );
            assert_eq!(
                health.inspection_only_document_count,
                expected_paths.len() as u64
            );
            for path in expected_paths {
                let ordinary = before
                    .document(&VaultRelativePath::new(path).unwrap())
                    .unwrap()
                    .unwrap();
                assert!(ordinary.record_id.is_none());
                assert!(ordinary.source_id.is_none());
                assert!(ordinary.owner_revision.is_none());
                assert_eq!(
                    ordinary.eligibility,
                    crate::domain::Eligibility::Unsupported
                );
                assert_eq!(ordinary.reasons, vec!["unadopted_note_inspection_only"]);
            }
            drop(before);
            let mut bytes = b"---\nwiki_id: before_limit\n#".to_vec();
            bytes.resize(crate::sources::identity::MAX_RESERVATION_BYTES + 16, b'x');
            bytes.extend_from_slice(b"\nwiki_id: after_limit\n");
            fs::write(app.fs.root().path().join("oversized.md"), &bytes).unwrap();
            assert_eq!(
                app.index_sync(false).unwrap_err().code,
                ErrorCode::BudgetExceeded
            );
            let after = app
                .catalog()
                .cached_query_snapshot(Default::default())
                .unwrap();
            assert_eq!(
                crate::catalog::query_types::QueryCatalog::snapshot(&after),
                &snapshot
            );
            assert!(
                after
                    .document(&VaultRelativePath::new("notes/ordinary.md").unwrap())
                    .unwrap()
                    .is_some()
            );
            drop(after);
            assert_eq!(
                crate::sources::SourceView::from_fs(&app.fs)
                    .err()
                    .unwrap()
                    .code,
                ErrorCode::BudgetExceeded
            );
            let inspected = app
                .inspect_literal(LiteralInspectRequest {
                    path: VaultRelativePath::new("oversized.md").unwrap(),
                    range: None,
                    max_bytes: MAX_INPUT_BYTES,
                })
                .unwrap();
            assert_eq!(inspected.bytes, bytes);
            assert!(!inspected.structured_currentness_asserted);
        }
    }

    #[test]
    fn page_literal_edit_preserves_identity_and_refuses_metadata_stripping() {
        for migrated in [false, true] {
            let (_temp, app) = fixture(true, migrated);
            app.page_initialize(
                Some(VaultRelativePath::new("pages/checklist.md").unwrap()),
                None,
                "Checklist".into(),
                "# Heading\nAuthor keeps amber.\n".into(),
            )
            .unwrap();
            let path = app.fs.root().path().join("pages/checklist.md");
            let before = fs::read(&path).unwrap();
            let id = parse_note(&before).canonical.unwrap().id().clone();
            let request = replace_word("pages/checklist.md", &before, b"amber", "violet");
            let after = splice_bytes(&before, &request.patch).unwrap();
            app.edit_literal(request).unwrap();
            assert_eq!(fs::read(&path).unwrap(), after);
            assert_eq!(parse_note(&after).canonical.unwrap().id(), &id);
            let bad = LiteralEditRequest {
                path: VaultRelativePath::new("pages/checklist.md").unwrap(),
                if_match: Blake3Hash::digest(&after),
                patch: patch(
                    ByteSpan::new(0, after.len() as u64).unwrap(),
                    "lost envelope\n",
                ),
            };
            assert!(app.edit_literal(bad).is_err());
            assert_eq!(fs::read(&path).unwrap(), after);
        }
    }

    #[test]
    fn explicit_source_refresh_path_preserves_input_and_immutable_history_both_layouts() {
        for migrated in [false, true] {
            let (_temp, app) = fixture(true, migrated);
            let input = app.fs.root().path().join("notes/ordinary.md");
            let original = fs::read(&input).unwrap();
            let capture = crate::sources::path_refresh::guarded_capture_file(
                &input,
                Some("Runbook"),
                Some("text/markdown".into()),
                &Blake3Hash::digest(&original),
            )
            .unwrap();
            let added = app.source_add(capture).unwrap();
            let source = added.allocated_ids["source"].clone();
            let revision = added.allocated_ids["revision"].clone();
            let selected_path =
                VaultRelativePath::new(format!("sources/{source}/source.md")).unwrap();
            let old_root = app
                .fs
                .root()
                .path()
                .join(format!("sources/{source}/revisions/{revision}"));
            let immutable = ["original.bin", "content.md", "revision.md"]
                .map(|name| (name, fs::read(old_root.join(name)).unwrap()));
            let changed = splice_bytes(
                &original,
                &replace_word("notes/ordinary.md", &original, b"amber", "violet").patch,
            )
            .unwrap();
            fs::write(&input, &changed).unwrap();
            assert_eq!(
                crate::sources::path_refresh::guarded_capture_file(
                    &input,
                    None,
                    None,
                    &Blake3Hash::digest(&original)
                )
                .unwrap_err()
                .code,
                ErrorCode::ContentConflict
            );
            let request = crate::sources::path_refresh::guarded_capture_file(
                &input,
                None,
                None,
                &Blake3Hash::digest(&changed),
            )
            .unwrap();
            let refreshed = app
                .source_refresh_path_with_title(selected_path.clone(), request, None)
                .unwrap();
            assert_ne!(refreshed.allocated_ids["revision"], revision);
            assert_eq!(refreshed.allocated_ids["source"], source);
            assert_eq!(fs::read(&input).unwrap(), changed);
            for (name, bytes) in immutable {
                assert_eq!(fs::read(old_root.join(name)).unwrap(), bytes);
            }
            let request = crate::sources::path_refresh::guarded_capture_file(
                &input,
                None,
                None,
                &Blake3Hash::digest(&changed),
            )
            .unwrap();
            let no_op = app
                .source_refresh_path_with_title(selected_path, request, None)
                .unwrap();
            assert!(no_op.reused);
            assert_eq!(
                no_op.allocated_ids["revision"],
                refreshed.allocated_ids["revision"]
            );
            assert!(
                app.source_refresh_path_with_title(
                    VaultRelativePath::new("notes/ordinary.md").unwrap(),
                    crate::sources::path_refresh::guarded_capture_file(
                        &input,
                        None,
                        None,
                        &Blake3Hash::digest(&changed)
                    )
                    .unwrap(),
                    None
                )
                .is_err()
            );
            app.index_rebuild_normalized().unwrap();
        }
    }

    #[test]
    fn malformed_wiki_and_immutable_notes_refuse_literal_edit_without_byte_loss() {
        let (_temp, app) = fixture(true, false);
        let path = app.fs.root().path().join("notes/damaged.md");
        let bytes = b"---\nwiki_schema: '1'\nwiki_id: damaged\nwiki_kind: page\ntitle: Damaged\nwiki_status: [\n---\nAuthor body stays.\n";
        fs::write(&path, bytes).unwrap();
        let request = replace_word("notes/damaged.md", bytes, b"body", "explanation");
        let error = app.edit_literal(request).unwrap_err();
        assert!(error.hint.is_some());
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(
            app.edit_literal(LiteralEditRequest {
                path: VaultRelativePath::new("sources/s/revisions/r/content.md").unwrap(),
                if_match: Blake3Hash::digest(b""),
                patch: patch(ByteSpan::new(0, 0).unwrap(), "tamper")
            })
            .is_err()
        );
    }
}

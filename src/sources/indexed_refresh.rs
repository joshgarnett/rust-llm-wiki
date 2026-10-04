//! Selected source refresh planning over a pinned published index.
//!
//! The index supplies managed identity/discovery facts. Only captured before
//! images are authenticated now; nonselected history belongs to explicit audit.
use super::{
    capture::{Extraction, draft, extract, revision_writes},
    revision::{canonical_path, dependencies, integrity},
    types::*,
};
use crate::{
    changes::{ExpectedWrite, ScanDocument},
    domain::{Blake3Hash, ErrorCode, RecordId, RecordKind, Result, VaultRelativePath, WikiError},
    records::{LinkResolution, ParsedNote, RegistryEntry, edit_note, parse_note, resolve_typed},
    vault::{ExpectedState, VaultFs},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::OpenOptions,
    io::Read,
};

#[cfg(test)]
thread_local! {
    static READ_PATHS: std::cell::RefCell<Vec<VaultRelativePath>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn budget(message: &str) -> WikiError {
    WikiError::new(ErrorCode::BudgetExceeded, message)
}

struct Captured<'a> {
    fs: &'a VaultFs,
    limits: &'a SourceRefreshLimits,
    read_bytes: usize,
    files: BTreeMap<VaultRelativePath, ScanDocument>,
}
impl<'a> Captured<'a> {
    /// Charge every read, including final dependency rereads, before allocation.
    fn read(&mut self, path: &VaultRelativePath) -> Result<Vec<u8>> {
        #[cfg(test)]
        READ_PATHS.with(|paths| paths.borrow_mut().push(path.clone()));
        let resolved = self.fs.root().resolve(path)?;
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
        }
        let mut file = options
            .open(&resolved)
            .map_err(|error| integrity(format!("selected refresh read {path}: {error}")))?;
        let meta = file
            .metadata()
            .map_err(|error| integrity(format!("selected refresh metadata {path}: {error}")))?;
        if !meta.is_file() {
            return Err(integrity("selected refresh input is not a regular file"));
        }
        let length = usize::try_from(meta.len())
            .map_err(|_| budget("selected refresh input length exceeds platform"))?;
        if length > self.limits.max_file_bytes {
            return Err(budget("selected refresh file exceeds byte ceiling"));
        }
        self.read_bytes = self
            .read_bytes
            .checked_add(length)
            .filter(|&sum| sum <= self.limits.max_canonical_bytes)
            .ok_or_else(|| budget("selected refresh reads exceed aggregate byte ceiling"))?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|_| budget("selected refresh allocation refused"))?;
        bytes.resize(length, 0);
        file.read_exact(&mut bytes).map_err(|error| {
            integrity(format!("selected refresh input changed {path}: {error}"))
        })?;
        let mut extra = [0];
        if file
            .read(&mut extra)
            .map_err(|error| integrity(format!("selected refresh read {path}: {error}")))?
            != 0
        {
            return Err(integrity("selected refresh input grew during capture"));
        }
        Ok(bytes)
    }

    fn capture(&mut self, path: &VaultRelativePath) -> Result<&ScanDocument> {
        if !self.files.contains_key(path) {
            let bytes = self.read(path)?;
            let hash = Blake3Hash::digest(&bytes);
            self.files.insert(
                path.clone(),
                ScanDocument {
                    path: path.clone(),
                    hash,
                    bytes,
                },
            );
        }
        Ok(self.files.get(path).expect("captured entry"))
    }

    fn authenticate(&mut self, record: &RefreshRecord, kind: RecordKind) -> Result<ParsedNote> {
        if record.record.kind() != kind || !canonical_path(&record.path) {
            return Err(integrity("selected refresh record kind or path disagrees"));
        }
        let document = self.capture(&record.path)?;
        if document.hash != record.hash {
            return Err(integrity("selected refresh record hash mismatch"));
        }
        let note = parse_note(&document.bytes);
        if note.canonical.as_ref() != Some(&record.record) {
            return Err(integrity(
                "selected refresh canonical record disagrees with index",
            ));
        }
        Ok(note)
    }

    fn finish(mut self) -> Result<(Vec<ScanDocument>, Vec<crate::changes::ReadDependency>)> {
        let expected: Vec<_> = self
            .files
            .values()
            .map(|document| (document.path.clone(), document.hash.clone()))
            .collect();
        for (path, hash) in expected {
            if Blake3Hash::digest(self.read(&path)?) != hash {
                return Err(WikiError::new(
                    ErrorCode::FreshnessConflict,
                    "selected refresh dependency changed during planning",
                ));
            }
        }
        let deps = dependencies(
            self.files
                .values()
                .map(|document| {
                    (
                        document.path.clone(),
                        ExpectedState::Hash(document.hash.clone()),
                    )
                })
                .collect(),
        );
        Ok((self.files.into_values().collect(), deps))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::ReadSnapshot,
        sources::revision::{common, record_bytes},
        vault::VaultRoot,
    };
    use std::{
        cell::{Cell, RefCell},
        fs,
        path::PathBuf,
    };

    struct Lookup {
        snapshot: ReadSnapshot,
        vault: RecordId,
        records: BTreeMap<RecordId, RefreshRecord>,
        unique_calls: RefCell<Vec<RecordId>>,
        matching_calls: Cell<usize>,
        matching_ordinal: Option<usize>,
        assertion_calls: Cell<usize>,
        ambiguous: Option<RecordId>,
        claimed: bool,
        reserved_attempts: Cell<usize>,
        reservation_calls: RefCell<Vec<(RecordId, VaultRelativePath)>>,
        mutate_on_claim: Option<PathBuf>,
    }
    impl SourceRefreshLookup for Lookup {
        fn snapshot(&self) -> &ReadSnapshot {
            &self.snapshot
        }
        fn vault_id(&self) -> &RecordId {
            &self.vault
        }
        fn unique_record(&self, id: &RecordId) -> Result<Option<RefreshRecord>> {
            self.unique_calls.borrow_mut().push(id.clone());
            if self.ambiguous.as_ref() == Some(id) {
                return Err(WikiError::new(
                    ErrorCode::ReferenceAmbiguous,
                    "malformed duplicate claim",
                ));
            }
            Ok(self.records.get(id).cloned())
        }
        fn id_is_claimed(&self, _: &RecordId) -> Result<bool> {
            if let Some(path) = &self.mutate_on_claim {
                fs::write(path, b"concurrent selected edit").unwrap();
            }
            Ok(self.claimed)
        }
        fn revision_identity_is_reserved(
            &self,
            id: &RecordId,
            path: &VaultRelativePath,
        ) -> Result<bool> {
            self.reservation_calls
                .borrow_mut()
                .push((id.clone(), path.clone()));
            if self.id_is_claimed(id)? {
                return Ok(true);
            }
            let remaining = self.reserved_attempts.get();
            self.reserved_attempts.set(remaining.saturating_sub(1));
            Ok(remaining > 0)
        }
        fn record_at_path(&self, path: &VaultRelativePath) -> Result<Option<RefreshRecord>> {
            Ok(self.records.values().find(|row| &row.path == path).cloned())
        }
        fn matching_revision(
            &self,
            source: &RecordId,
            signature: &RevisionSignature,
        ) -> Result<Option<MatchingRevision>> {
            self.matching_calls.set(self.matching_calls.get() + 1);
            for (ordinal, id) in self.records[source]
                .record
                .field("wiki_revisions")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .enumerate()
            {
                let row = &self.records[&RecordId::new(id.as_str().unwrap()).unwrap()];
                if row.record.string("wiki_original_hash") == Some(signature.original_hash.as_str())
                    && row.record.string("wiki_content_hash")
                        == signature.content_hash.as_ref().map(Blake3Hash::as_str)
                    && row.record.string("wiki_extractor_fingerprint")
                        == Some(signature.extractor_fingerprint.as_str())
                {
                    return Ok(Some(MatchingRevision {
                        revision: row.clone(),
                        retained_ordinal: self.matching_ordinal.unwrap_or(ordinal),
                    }));
                }
            }
            Ok(None)
        }
        fn source_assertions(&self, _: &RecordId) -> Result<Vec<RecordId>> {
            self.assertion_calls.set(self.assertion_calls.get() + 1);
            Ok(vec![RecordId::new("assertion_selected").unwrap(); 2])
        }
    }

    struct Fixture {
        temp: tempfile::TempDir,
        store: SourceStore,
        vault: RecordId,
        source: RecordId,
        initial: RecordId,
    }
    impl Fixture {
        fn new(request: CaptureRequest) -> Self {
            let temp = tempfile::tempdir().unwrap();
            let vault = RecordId::generate(RecordKind::Vault).unwrap();
            fs::write(
                temp.path().join("WIKI.md"),
                record_bytes(
                    crate::domain::CanonicalRecord::new(common(
                        &vault,
                        RecordKind::Vault,
                        "Refresh fixture",
                    ))
                    .unwrap(),
                    b"",
                )
                .unwrap(),
            )
            .unwrap();
            let store = SourceStore::new(VaultFs::new(VaultRoot::explicit(temp.path()).unwrap()));
            let plan = store.plan_capture(request).unwrap();
            let fixture = Self {
                temp,
                store,
                vault,
                source: plan.source_id.clone(),
                initial: plan.revision_id.clone(),
            };
            fixture.apply(plan);
            fixture
        }
        fn apply(&self, plan: SourcePlan) {
            if let Some(draft) = plan.draft {
                for op in draft.operations {
                    let path = self.temp.path().join(op.target.as_str());
                    fs::create_dir_all(path.parent().unwrap()).unwrap();
                    fs::write(path, op.proposed.unwrap()).unwrap();
                }
            }
        }
        fn refresh(&self, request: CaptureRequest) -> RecordId {
            let plan = self.store.plan_refresh(&self.source, request).unwrap();
            let revision = plan.revision_id.clone();
            self.apply(plan);
            revision
        }
        fn duplicate_revision(&self, request: CaptureRequest, make_head: bool) -> RecordId {
            let revision = RecordId::generate(RecordKind::Revision).unwrap();
            let path =
                VaultRelativePath::new(format!("sources/{}/source.md", self.source)).unwrap();
            let note = parse_note(&fs::read(self.temp.path().join(path.as_str())).unwrap());
            let mut operations = revision_writes(
                &self.source,
                &revision,
                &path,
                &request,
                &extract(&request).unwrap(),
            )
            .unwrap();
            let mut retained = note
                .canonical
                .as_ref()
                .unwrap()
                .field("wiki_revisions")
                .unwrap()
                .as_array()
                .unwrap()
                .clone();
            retained.push(revision.as_str().into());
            let mut changes =
                BTreeMap::from([("wiki_revisions".into(), serde_json::Value::Array(retained))]);
            if make_head {
                changes.insert("wiki_current_revision".into(), revision.as_str().into());
                changes.insert(
                    "wiki_revision".into(),
                    format!(
                        "[[sources/{}/revisions/{revision}/revision.md]]",
                        self.source
                    )
                    .into(),
                );
            }
            operations.push(ExpectedWrite {
                target: path,
                expected: ExpectedState::Hash(note.source_hash.clone()),
                proposed: Some(edit_note(&note, &changes, None, &note.source_hash).unwrap()),
                apply_after: vec![],
            });
            self.apply(SourcePlan {
                draft: Some(draft(
                    "Imported identical retained capture".into(),
                    operations,
                    BTreeMap::new(),
                )),
                source_id: self.source.clone(),
                revision_id: revision.clone(),
                reused: false,
                invalidation: InvalidationInputs::default(),
                dependencies: vec![],
                capture_state: None,
            });
            revision
        }
        fn lookup(&self) -> Lookup {
            let records = self
                .store
                .fs
                .root()
                .scan_markdown()
                .unwrap()
                .into_iter()
                .filter_map(|path| {
                    let bytes = fs::read(self.temp.path().join(path.as_str())).unwrap();
                    parse_note(&bytes).canonical.map(|record| {
                        (
                            record.id().clone(),
                            RefreshRecord {
                                record,
                                path,
                                hash: Blake3Hash::digest(bytes),
                            },
                        )
                    })
                })
                .collect();
            Lookup {
                snapshot: ReadSnapshot::published(
                    1,
                    crate::catalog::scan::parser_fingerprint(),
                    "a".repeat(32),
                    Blake3Hash::digest(b"published fixture"),
                )
                .unwrap(),
                vault: self.vault.clone(),
                records,
                unique_calls: RefCell::new(vec![]),
                matching_calls: Cell::new(0),
                matching_ordinal: None,
                assertion_calls: Cell::new(0),
                ambiguous: None,
                claimed: false,
                reserved_attempts: Cell::new(0),
                reservation_calls: RefCell::new(vec![]),
                mutate_on_claim: None,
            }
        }
        fn plan(
            &self,
            lookup: &Lookup,
            request: CaptureRequest,
            title: Option<&str>,
        ) -> Result<IndexedSourceRefreshPlan> {
            READ_PATHS.with(|paths| paths.borrow_mut().clear());
            self.store.plan_refresh_indexed(
                lookup,
                &self.source,
                request,
                title,
                &SourceRefreshLimits::default(),
            )
        }
        fn asset(&self, revision: &RecordId, file: &str) -> PathBuf {
            self.temp.path().join(format!(
                "sources/{}/revisions/{revision}/{file}",
                self.source
            ))
        }
    }
    fn request(bytes: &[u8]) -> CaptureRequest {
        CaptureRequest {
            title: "Original canonical title".into(),
            origin_kind: SourceOrigin::LocalFile,
            origin: "fixture.md".into(),
            original: bytes.to_vec(),
            extraction: ExtractionInput::Utf8Preserve,
            media_type: Some("text/markdown".into()),
        }
    }

    #[test]
    fn noop_prefers_current_head_without_history_or_unrelated_reads() {
        let fixture = Fixture::new(request(b"old archival bytes"));
        let head = fixture.refresh(request("new café 東京 bytes".as_bytes()));
        let lookup = fixture.lookup();
        fs::remove_file(fixture.asset(&fixture.initial, "original.bin")).unwrap();
        fs::write(
            fixture.asset(&fixture.initial, "content.md"),
            b"damaged unselected history",
        )
        .unwrap();
        fs::write(
            fixture.temp.path().join("unrelated.md"),
            b"---\nwiki_id: malformed\n\xff",
        )
        .unwrap();
        let mut changed_default = request("new café 東京 bytes".as_bytes());
        changed_default.title = "new filename default".into();
        let plan = fixture.plan(&lookup, changed_default, None).unwrap();
        assert!(plan.plan.draft.is_none());
        assert!(plan.plan.reused);
        assert_eq!(plan.plan.revision_id, head);
        assert_eq!(lookup.matching_calls.get(), 0);
        assert_eq!(lookup.assertion_calls.get(), 0);
        assert_eq!(lookup.unique_calls.borrow().len(), 3);
        assert_eq!(plan.captured.len(), 5);
        assert_eq!(plan.plan.dependencies.len(), plan.captured.len());
        READ_PATHS.with(|paths| {
            assert_eq!(paths.borrow().len(), 10);
            assert!(
                paths
                    .borrow()
                    .iter()
                    .all(|path| plan.captured.iter().any(|doc| &doc.path == path))
            );
            assert!(
                !paths
                    .borrow()
                    .iter()
                    .any(|path| path.as_str().contains(fixture.initial.as_str()))
            );
        });
    }

    #[test]
    fn title_only_edits_source_and_historical_reuse_preserves_immutable_revision() {
        let fixture = Fixture::new(request(b"original"));
        let head = fixture.refresh(request(b"current"));
        let lookup = fixture.lookup();
        let title = fixture
            .plan(&lookup, request(b"current"), Some("Explicit title"))
            .unwrap();
        let draft = title.plan.draft.unwrap();
        assert_eq!(draft.operations.len(), 1);
        let note = parse_note(draft.operations[0].proposed.as_ref().unwrap());
        assert_eq!(note.canonical.unwrap().title(), "Explicit title");
        assert!(draft.allocated_ids.is_empty());
        assert_eq!(title.previous_revision, head);
        assert!(title.plan.invalidation.assertion_ids.is_empty());
        assert_eq!(lookup.assertion_calls.get(), 0);
        let history = fixture.plan(&lookup, request(b"original"), None).unwrap();
        assert_eq!(history.plan.revision_id, fixture.initial);
        assert!(history.plan.reused);
        assert_eq!(history.plan.draft.as_ref().unwrap().operations.len(), 1);
        assert_eq!(history.captured.len(), 8);
        assert_eq!(history.previous_revision, head);
        assert_eq!(history.base_snapshot, lookup.snapshot);
        assert!(history.plan.invalidation.assertion_ids.is_empty());
        assert_eq!(lookup.assertion_calls.get(), 0);
    }

    #[test]
    fn changed_bytes_allocate_absent_assets_then_manifest_then_head() {
        let fixture = Fixture::new(request(b"old"));
        let lookup = fixture.lookup();
        let plan = fixture.plan(&lookup, request(b"new"), None).unwrap();
        assert!(!plan.plan.reused);
        assert!(plan.plan.invalidation.assertion_ids.is_empty());
        assert_eq!(lookup.assertion_calls.get(), 0);
        let draft = plan.plan.draft.unwrap();
        assert_eq!(draft.operations.len(), 4);
        assert!(
            draft.operations[0]
                .target
                .as_str()
                .ends_with("original.bin")
        );
        assert!(draft.operations[1].target.as_str().ends_with("content.md"));
        assert!(draft.operations[2].target.as_str().ends_with("revision.md"));
        assert_eq!(draft.operations[2].apply_after.len(), 2);
        assert_eq!(draft.operations[3].apply_after.len(), 3);
        assert!(
            draft.operations[..3]
                .iter()
                .all(|op| op.expected == ExpectedState::Absent)
        );
        assert_eq!(draft.read_preconditions, plan.plan.dependencies);
    }

    #[test]
    fn first_retained_match_and_current_head_preference_remain_distinct() {
        let fixture = Fixture::new(request(b"identical old bytes"));
        fixture.refresh(request(b"different head"));
        fixture.duplicate_revision(request(b"identical old bytes"), false);
        let plan = fixture
            .plan(&fixture.lookup(), request(b"identical old bytes"), None)
            .unwrap();
        assert_eq!(plan.plan.revision_id, fixture.initial);
        let current = fixture.duplicate_revision(request(b"identical old bytes"), true);
        let lookup = fixture.lookup();
        fs::remove_file(fixture.asset(&fixture.initial, "original.bin")).unwrap();
        let plan = fixture
            .plan(&lookup, request(b"identical old bytes"), None)
            .unwrap();
        assert!(plan.plan.draft.is_none());
        assert_eq!(plan.plan.revision_id, current);
        assert_eq!(lookup.matching_calls.get(), 0);
    }

    #[test]
    fn matching_revision_ordinal_must_bind_exact_retained_position() {
        let fixture = Fixture::new(request(b"historical match"));
        fixture.refresh(request(b"different current head"));
        for ordinal in [1, 2] {
            let mut lookup = fixture.lookup();
            lookup.matching_ordinal = Some(ordinal);
            let error = fixture
                .plan(&lookup, request(b"historical match"), None)
                .err()
                .unwrap();
            assert_eq!(error.code, ErrorCode::SourceIntegrity);
            assert!(error.message.contains("retained ordinal"));
            assert_eq!(lookup.unique_calls.borrow().len(), 3);
            READ_PATHS.with(|paths| {
                assert!(
                    !paths
                        .borrow()
                        .iter()
                        .any(|path| { path.as_str().contains(fixture.initial.as_str()) })
                );
            });
        }
    }

    #[test]
    fn stale_companion_is_allowed_but_known_conflicting_path_refuses() {
        let fixture = Fixture::new(request(b"original"));
        let path = fixture
            .temp
            .path()
            .join(format!("sources/{}/source.md", fixture.source));
        for (companion, succeeds) in [
            ("[[missing/path#heading|label]]", true),
            ("[[WIKI.md]]", false),
        ] {
            let note = parse_note(&fs::read(&path).unwrap());
            let changes = BTreeMap::from([("wiki_revision".into(), companion.into())]);
            fs::write(
                &path,
                edit_note(&note, &changes, None, &note.source_hash).unwrap(),
            )
            .unwrap();
            let result = fixture.plan(&fixture.lookup(), request(b"original"), None);
            assert_eq!(result.is_ok(), succeeds);
            if !succeeds {
                assert_eq!(result.err().unwrap().code, ErrorCode::SourceIntegrity);
            }
        }
    }

    #[test]
    fn unsupported_and_empty_content_remain_distinct_and_reusable() {
        for original in [vec![], vec![0xff, 0xfe]] {
            let fixture = Fixture::new(request(&original));
            let plan = fixture
                .plan(&fixture.lookup(), request(&original), None)
                .unwrap();
            assert!(plan.plan.draft.is_none());
            assert_eq!(
                plan.plan.capture_state,
                Some(if original.is_empty() {
                    SourceCaptureState::Empty
                } else {
                    SourceCaptureState::Unsupported
                })
            );
            assert_eq!(plan.captured.len(), if original.is_empty() { 5 } else { 4 });
        }
    }

    #[test]
    fn selected_tamper_ambiguity_and_id_collision_refuse() {
        let fixture = Fixture::new(request(b"original"));
        let mut lookup = fixture.lookup();
        lookup.ambiguous = Some(fixture.source.clone());
        assert_eq!(
            fixture
                .plan(&lookup, request(b"new"), None)
                .err()
                .unwrap()
                .code,
            ErrorCode::ReferenceAmbiguous
        );
        lookup.ambiguous = None;
        lookup.claimed = true;
        assert_eq!(
            fixture
                .plan(&lookup, request(b"new"), None)
                .err()
                .unwrap()
                .code,
            ErrorCode::ReferenceAmbiguous
        );
        lookup.claimed = false;
        fs::write(fixture.asset(&fixture.initial, "content.md"), b"changed").unwrap();
        assert_eq!(
            fixture
                .plan(&lookup, request(b"original"), None)
                .err()
                .unwrap()
                .code,
            ErrorCode::SourceIntegrity
        );
    }

    #[test]
    fn generated_revision_retries_reserved_id_or_path_without_scanning_dependents() {
        let fixture = Fixture::new(request(b"original"));
        let lookup = fixture.lookup();
        lookup.reserved_attempts.set(2);
        let plan = fixture.plan(&lookup, request(b"new bytes"), None).unwrap();
        let calls = lookup.reservation_calls.borrow();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[2].0, plan.plan.revision_id);
        assert_eq!(
            calls
                .iter()
                .map(|(id, _)| id)
                .collect::<BTreeSet<_>>()
                .len(),
            3
        );
        for (id, path) in calls.iter() {
            assert_eq!(
                path.as_str(),
                format!("sources/{}/revisions/{id}/revision.md", fixture.source)
            );
        }
        assert_eq!(lookup.assertion_calls.get(), 0);
        drop(calls);
        let mut exhausted = fixture.lookup();
        exhausted.claimed = true;
        assert_eq!(
            fixture
                .plan(&exhausted, request(b"other bytes"), None)
                .err()
                .unwrap()
                .code,
            ErrorCode::ReferenceAmbiguous
        );
        assert_eq!(exhausted.reservation_calls.borrow().len(), 16);
        let noop = fixture.lookup();
        fixture.plan(&noop, request(b"original"), None).unwrap();
        assert!(noop.reservation_calls.borrow().is_empty());
    }

    #[test]
    fn final_selected_dependency_edit_refuses() {
        let fixture = Fixture::new(request(b"original"));
        let mut lookup = fixture.lookup();
        lookup.mutate_on_claim = Some(fixture.asset(&fixture.initial, "content.md"));
        assert_eq!(
            fixture
                .plan(&lookup, request(b"new"), None)
                .err()
                .unwrap()
                .code,
            ErrorCode::FreshnessConflict
        );
    }

    #[test]
    fn byte_and_identity_list_budgets_precede_payload_allocation() {
        let fixture = Fixture::new(request(&vec![b'x'; 4096]));
        let lookup = fixture.lookup();
        let mut limits = SourceRefreshLimits::default();
        limits.max_file_bytes = 2048;
        assert_eq!(
            fixture
                .store
                .plan_refresh_indexed(&lookup, &fixture.source, request(b"new"), None, &limits)
                .err()
                .unwrap()
                .code,
            ErrorCode::BudgetExceeded
        );
        limits = SourceRefreshLimits::default();
        limits.max_canonical_bytes = 1;
        assert_eq!(
            fixture
                .store
                .plan_refresh_indexed(&lookup, &fixture.source, request(b"new"), None, &limits)
                .err()
                .unwrap()
                .code,
            ErrorCode::BudgetExceeded
        );
        fixture.refresh(request(b"second"));
        let lookup = fixture.lookup();
        limits = SourceRefreshLimits::default();
        limits.max_retained_revisions = 1;
        assert_eq!(
            fixture
                .store
                .plan_refresh_indexed(&lookup, &fixture.source, request(b"new"), None, &limits)
                .err()
                .unwrap()
                .code,
            ErrorCode::BudgetExceeded
        );
    }

    #[test]
    fn unpublished_or_wrong_parser_and_vault_binding_refuse() {
        let fixture = Fixture::new(request(b"original"));
        let mut lookup = fixture.lookup();
        lookup.snapshot.parser_fingerprint = Blake3Hash::digest(b"wrong parser");
        assert_eq!(
            fixture
                .plan(&lookup, request(b"original"), None)
                .err()
                .unwrap()
                .code,
            ErrorCode::OfflineUnavailable
        );
        lookup.snapshot = ReadSnapshot::canonical(
            1,
            crate::catalog::scan::parser_fingerprint(),
            Blake3Hash::digest(b"canonical audit"),
        );
        assert_eq!(
            fixture
                .plan(&lookup, request(b"original"), None)
                .err()
                .unwrap()
                .code,
            ErrorCode::OfflineUnavailable
        );
        let mut lookup = fixture.lookup();
        lookup.records.get_mut(&fixture.vault).unwrap().path =
            VaultRelativePath::new("other.md").unwrap();
        assert_eq!(
            fixture
                .plan(&lookup, request(b"original"), None)
                .err()
                .unwrap()
                .code,
            ErrorCode::SourceIntegrity
        );
    }
}

fn unique(
    lookup: &dyn SourceRefreshLookup,
    id: &RecordId,
    kind: RecordKind,
) -> Result<RefreshRecord> {
    let row = lookup.unique_record(id)?.ok_or_else(|| {
        WikiError::new(
            ErrorCode::RecordNotFound,
            format!("missing selected {kind} {id}"),
        )
    })?;
    if row.record.id() != id || row.record.kind() != kind {
        return Err(integrity(
            "selected lookup returned another identity or kind",
        ));
    }
    Ok(row)
}

/// Preserve typed navigation semantics: omitted/missing companions are stale,
/// while an existing different identity at an exact path is a conflict.
fn companion(
    lookup: &dyn SourceRefreshLookup,
    target: &RefreshRecord,
    value: Option<&str>,
) -> Result<()> {
    let mut registry = vec![RegistryEntry {
        id: target.record.id().clone(),
        kind: target.record.kind(),
        path: target.path.clone(),
        aliases: vec![],
    }];
    if let Some(value) = value {
        let path = value
            .strip_prefix("[[")
            .and_then(|value| value.strip_suffix("]]"))
            .unwrap_or(value)
            .split(['|', '#'])
            .next()
            .unwrap_or_default();
        if let Ok(path) = VaultRelativePath::new(path) {
            let direct = lookup.record_at_path(&path)?;
            if direct.as_ref().is_some_and(|row| row.path != path) {
                return Err(integrity("companion lookup returned a different path"));
            }
            let at_path = match direct {
                Some(row) => Some(row),
                None if !path.as_str().ends_with(".md") => {
                    let extended = VaultRelativePath::new(format!("{path}.md"))?;
                    let row = lookup.record_at_path(&extended)?;
                    if row.as_ref().is_some_and(|row| row.path != extended) {
                        return Err(integrity("companion lookup returned a different path"));
                    }
                    row
                }
                None => None,
            };
            if let Some(row) = at_path {
                if row.record.id() == target.record.id() {
                    if row.path != target.path
                        || row.hash != target.hash
                        || row.record != target.record
                    {
                        return Err(integrity(
                            "selected companion identity disagrees with index",
                        ));
                    }
                } else {
                    registry.push(RegistryEntry {
                        id: row.record.id().clone(),
                        kind: row.record.kind(),
                        path: row.path,
                        aliases: vec![],
                    });
                }
            }
        }
    }
    match resolve_typed(&registry, target.record.id(), target.record.kind(), value) {
        LinkResolution::Resolved { .. } => Ok(()),
        _ => Err(integrity(
            "selected refresh companion conflicts with its identity",
        )),
    }
}

fn revision_matches(
    captured: &mut Captured<'_>,
    lookup: &dyn SourceRefreshLookup,
    source: &RefreshRecord,
    revision: &RefreshRecord,
    request: &CaptureRequest,
    extraction: &Extraction,
) -> Result<bool> {
    captured.authenticate(revision, RecordKind::Revision)?;
    if revision.record.string("wiki_source_id") != Some(source.record.id().as_str()) {
        return Err(integrity("selected retained revision ownership mismatch"));
    }
    companion(lookup, source, revision.record.string("wiki_source"))?;
    let parent = revision
        .path
        .as_str()
        .rsplit_once('/')
        .ok_or_else(|| integrity("selected revision has no parent"))?
        .0;
    let original = captured.capture(&VaultRelativePath::new(format!(
        "{parent}/{}",
        revision
            .record
            .string("wiki_original_path")
            .expect("canonical original path")
    ))?)?;
    if Some(original.hash.as_str()) != revision.record.string("wiki_original_hash") {
        return Err(integrity("selected revision original hash mismatch"));
    }
    let original_matches = original.bytes == request.original;
    let content_matches = if revision.record.string("wiki_extraction_status") == Some("complete") {
        let content = captured.capture(&VaultRelativePath::new(format!(
            "{parent}/{}",
            revision
                .record
                .string("wiki_content_path")
                .expect("canonical content path")
        ))?)?;
        if Some(content.hash.as_str()) != revision.record.string("wiki_content_hash") {
            return Err(integrity("selected revision content hash mismatch"));
        }
        std::str::from_utf8(&content.bytes)
            .map_err(|_| integrity("selected extracted content is not UTF-8"))?;
        extraction.content.as_deref() == Some(content.bytes.as_slice())
    } else {
        extraction.content.is_none()
    };
    Ok(original_matches
        && content_matches
        && revision.record.string("wiki_extractor_fingerprint")
            == Some(extraction.fingerprint.as_str()))
}

impl SourceStore {
    pub(crate) fn plan_refresh_indexed(
        &self,
        lookup: &dyn SourceRefreshLookup,
        source_id: &RecordId,
        mut request: CaptureRequest,
        explicit_title: Option<&str>,
        limits: &SourceRefreshLimits,
    ) -> Result<IndexedSourceRefreshPlan> {
        if limits.max_file_bytes == 0
            || limits.max_file_bytes > 64 * 1024 * 1024
            || limits.max_canonical_bytes == 0
            || limits.max_canonical_bytes > 256 * 1024 * 1024
            || limits.max_retained_revisions == 0
            || limits.max_retained_revisions > 100_000
        {
            return Err(budget("invalid selected refresh limits"));
        }
        if request.original.len() > limits.max_file_bytes
            || matches!(&request.extraction, ExtractionInput::Supplied { content, .. } if content.len() > limits.max_file_bytes)
        {
            return Err(budget(
                "refresh input exceeds byte ceiling before extraction",
            ));
        }
        if lookup.snapshot().publication().is_none()
            || lookup.snapshot().parser_fingerprint != crate::catalog::scan::parser_fingerprint()
        {
            return Err(WikiError::new(
                ErrorCode::OfflineUnavailable,
                "indexed refresh requires a compatible published snapshot",
            ));
        }
        let base_snapshot = lookup.snapshot().clone();
        let mut captured = Captured {
            fs: &self.fs,
            limits,
            read_bytes: 0,
            files: BTreeMap::new(),
        };
        let vault = unique(lookup, lookup.vault_id(), RecordKind::Vault)?;
        if vault.path.as_str() != "WIKI.md" {
            return Err(integrity(
                "published vault identity is not bound to WIKI.md",
            ));
        }
        captured.authenticate(&vault, RecordKind::Vault)?;
        let source = unique(lookup, source_id, RecordKind::Source)?;
        let source_note = captured.authenticate(&source, RecordKind::Source)?;
        let retained = source
            .record
            .field("wiki_revisions")
            .and_then(serde_json::Value::as_array)
            .expect("canonical retained list");
        if retained.len() > limits.max_retained_revisions {
            return Err(budget("selected source retained revisions exceed ceiling"));
        }
        let revision_ids = retained
            .iter()
            .map(|value| RecordId::new(value.as_str().expect("canonical revision ID")))
            .collect::<Result<Vec<_>>>()?;
        let ids: BTreeSet<_> = revision_ids.iter().cloned().collect();
        let old_head = RecordId::new(
            source
                .record
                .string("wiki_current_revision")
                .expect("canonical head"),
        )?;
        if ids.len() != retained.len() || !ids.contains(&old_head) {
            return Err(integrity(
                "source has duplicate revisions or an unretained head",
            ));
        }
        let title = match explicit_title {
            Some(title) if title.trim().is_empty() => {
                return Err(WikiError::invalid("source title must not be empty"));
            }
            Some(title) => title,
            None => source.record.title(),
        };
        let title_changed = title != source.record.title();
        request.title = title.to_owned();
        let extraction = extract(&request)?;
        let signature = RevisionSignature {
            original_hash: Blake3Hash::digest(&request.original),
            content_hash: extraction.content.as_deref().map(Blake3Hash::digest),
            extractor_fingerprint: extraction.fingerprint.clone(),
        };
        let head = unique(lookup, &old_head, RecordKind::Revision)?;
        companion(lookup, &head, source.record.string("wiki_revision"))?;
        let reused =
            if revision_matches(&mut captured, lookup, &source, &head, &request, &extraction)? {
                Some(head)
            } else if let Some(candidate) = lookup.matching_revision(source_id, &signature)? {
                if revision_ids.get(candidate.retained_ordinal)
                    != Some(candidate.revision.record.id())
                {
                    return Err(integrity(
                        "matching revision retained ordinal disagrees with canonical source",
                    ));
                }
                let candidate = candidate.revision;
                if candidate.record.kind() != RecordKind::Revision
                    || !ids.contains(candidate.record.id())
                {
                    return Err(integrity(
                        "matching revision is not retained by selected source",
                    ));
                }
                let unique_candidate = unique(lookup, candidate.record.id(), RecordKind::Revision)?;
                if candidate.path != unique_candidate.path
                    || candidate.hash != unique_candidate.hash
                    || candidate.record != unique_candidate.record
                {
                    return Err(integrity(
                        "matching revision disagrees with unique indexed identity",
                    ));
                }
                if !revision_matches(
                    &mut captured,
                    lookup,
                    &source,
                    &candidate,
                    &request,
                    &extraction,
                )? {
                    return Err(integrity(
                        "matching revision signature disagrees with authenticated bytes",
                    ));
                }
                Some(candidate)
            } else {
                None
            };
        let is_reused = reused.is_some();
        let revision_id =
            match &reused {
                Some(revision) => revision.record.id().clone(),
                None => {
                    let mut allocated = None;
                    for _ in 0..16 {
                        let id = RecordId::generate(RecordKind::Revision)?;
                        let path = VaultRelativePath::new(format!(
                            "sources/{source_id}/revisions/{id}/revision.md"
                        ))?;
                        if !lookup.revision_identity_is_reserved(&id, &path)? {
                            allocated = Some(id);
                            break;
                        }
                    }
                    allocated.ok_or_else(|| WikiError::new(
                    ErrorCode::ReferenceAmbiguous,
                    "could not allocate an unreferenced revision identity after 16 attempts",
                ))?
                }
            };
        let unchanged = is_reused && revision_id == old_head && !title_changed;
        let (change, invalidation) = if unchanged {
            (None, InvalidationInputs::default())
        } else {
            let mut operations = if is_reused {
                vec![]
            } else {
                revision_writes(source_id, &revision_id, &source.path, &request, &extraction)?
            };
            let revision_path =
                reused
                    .as_ref()
                    .map(|row| row.path.clone())
                    .unwrap_or(VaultRelativePath::new(format!(
                        "sources/{source_id}/revisions/{revision_id}/revision.md"
                    ))?);
            let mut revisions = retained.clone();
            if !is_reused {
                revisions.push(revision_id.as_str().into());
            }
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
            operations.push(ExpectedWrite {
                target: source.path.clone(),
                expected: ExpectedState::Hash(source.hash.clone()),
                proposed: Some(edit_note(&source_note, &changes, None, &source.hash)?),
                apply_after: operations.iter().map(|op| op.target.clone()).collect(),
            });
            // The semantic projector discovers complete affected groups from
            // old/new-head relationships. Enumerating all source assertions here
            // would read unrelated historical support even for a title edit.
            // Empty is an unpopulated hint, never proof that fanout is empty.
            let assertions = Vec::new();
            (
                Some(draft(
                    format!("Refresh {}", source.record.title()),
                    operations,
                    if is_reused {
                        BTreeMap::new()
                    } else {
                        BTreeMap::from([("revision".into(), revision_id.clone())])
                    },
                )),
                InvalidationInputs {
                    source_ids: vec![source_id.clone()],
                    revision_ids,
                    assertion_ids: assertions,
                },
            )
        };
        let (captured, deps) = captured.finish()?;
        let mut change = change;
        if let Some(change) = &mut change {
            change.read_preconditions = deps.clone();
        }
        Ok(IndexedSourceRefreshPlan {
            plan: SourcePlan {
                draft: change,
                source_id: source_id.clone(),
                revision_id,
                reused: is_reused,
                invalidation,
                dependencies: deps,
                capture_state: Some(extraction.capture_state()),
            },
            base_snapshot,
            previous_revision: old_head,
            captured,
        })
    }
}

//! Ordinary automatic context must preserve conditions with their original evidence units.
use super::{
    ContextCheckpoint, ContextFault, ContextOptions, ContextRequest, ContextResult, ContextScope,
    ExcerptLabel, context, indexed_documents,
};
use crate::{
    app::{OfflineApp, OperationOptions, PageBatchRequest, PageUpdate, offline::init},
    catalog::{Catalog, SnapshotVerification},
    changes::{ChangeStatus, ReadDependency},
    domain::*,
    sources::{CaptureRequest, ExtractionInput, SourceOrigin},
    vault::{ExpectedState, VaultFs, VaultRoot},
};
use std::{fs, path::PathBuf, sync::Arc};

const QUERY: &str = "restart approval isolation pressure signed log checklist";
const CONDITION: &str = "Restart approval is valid only after isolation is complete and the pressure log is signed by the duty engineer. A verbal assurance does not satisfy either condition. The sign-off must identify the isolated valve and the measured pressure before the restart begins; a signature copied from an earlier shift is not valid. Keep the original log with the current maintenance record so the person authorizing the restart can verify the measurement and isolation together. Café operators must stop the restart if either record is missing.";
const CHECKLIST: &str = "- Isolation evidence: record the valve identifier and attach the isolation certificate to the signed pressure log. The certificate must describe this maintenance operation rather than a previous intervention; the engineer checks its identifier before approving the restart and keeps both records together for inspection.\n- Pressure evidence: wait 18 minutes after isolation, record the pressure in kilopascals, and obtain the duty engineer's signature before restart approval. A reading taken before the wait has elapsed is insufficient even if the pressure looks normal; repeat the observation after the full interval and retain the signed reading.";
fn body(wait: &str) -> String {
    format!(
        "# Restart manual\n\n## Authorized restart\n\n{CONDITION}\n\n{}\n\n## Background\n\nThe workshop noticeboard lists cleaning rotas.\n",
        CHECKLIST.replace("18 minutes", wait)
    )
}
fn path(value: &str) -> VaultRelativePath {
    VaultRelativePath::new(value).unwrap()
}
fn capture(text: &str) -> CaptureRequest {
    CaptureRequest {
        title: "Workshop restart instructions".into(),
        origin_kind: SourceOrigin::LocalFile,
        origin: "disposable-workshop.md".into(),
        original: text.as_bytes().to_vec(),
        extraction: ExtractionInput::Utf8Preserve,
        media_type: Some("text/markdown".into()),
    }
}
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    app: OfflineApp,
    catalog: Catalog,
    source: RecordId,
    first_revision: RecordId,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("group workflow vault");
        let options = OperationOptions {
            offline: true,
            lock_timeout_ms: 1000,
            ..Default::default()
        };
        init(&root, "Structural workflow", options).unwrap();
        let app =
            OfflineApp::new(VaultFs::new(VaultRoot::explicit(&root).unwrap()), options).unwrap();
        app.index_rebuild_normalized().unwrap();
        let added = app.source_add(capture(&body("18 minutes"))).unwrap();
        assert_eq!(added.status, Some(ChangeStatus::Committed));
        let source = added.allocated_ids["source"].clone();
        let first_revision = added.allocated_ids["revision"].clone();
        let catalog = Catalog::new(app.fs().clone(), app.vault_id().clone());
        Self {
            _temp: temp,
            root,
            app,
            catalog,
            source,
            first_revision,
        }
    }
    fn payload(&self, revision: &RecordId) -> VaultRelativePath {
        path(&format!(
            "sources/{}/revisions/{revision}/content.md",
            self.source
        ))
    }
    fn source_path(&self) -> VaultRelativePath {
        path(&format!("sources/{}/source.md", self.source))
    }
    fn request(&self) -> ContextRequest {
        let mut request = ContextRequest {
            scope: ContextScope::IndexedDocuments,
            ..Default::default()
        };
        request.documents.filters.source_ids = vec![self.source.clone()];
        request.documents.limits.hits = 5;
        request.documents.limits.candidates = 80;
        request.documents.limits.excerpt_bytes = 1024;
        request.budget.max_bytes = 6000;
        request.budget.max_tokens = 1500;
        request.verification_budget.max_elapsed_ms = 30_000;
        request
    }
    fn context(&self) -> ContextResult {
        indexed_documents::context(
            &self.catalog,
            QUERY,
            &self.request(),
            &ContextOptions::default(),
        )
        .unwrap()
    }
    fn dependency(&self, target: VaultRelativePath) -> ReadDependency {
        ReadDependency {
            expected: ExpectedState::Hash(Blake3Hash::digest(
                &fs::read(self.root.join(target.as_str())).unwrap(),
            )),
            path: target,
        }
    }
    fn page_request(
        &self,
        result: &ContextResult,
        revision: &RecordId,
        author: Option<Blake3Hash>,
    ) -> PageBatchRequest {
        let markdown = format!(
            "---\nwiki_schema: '1'\nwiki_id: page_restart_group\nwiki_kind: page\nwiki_status: reviewed\ntitle: Restart evidence\n---\n# Restart evidence\n\n{}",
            result.text()
        );
        PageBatchRequest {
            title: "Save exact restart evidence".into(),
            pages: vec![PageUpdate {
                path: path("pages/restart.md"),
                markdown,
                if_match: author,
            }],
            read_preconditions: vec![
                self.dependency(self.source_path()),
                self.dependency(self.payload(revision)),
            ],
        }
    }
}
fn authenticate(
    fixture: &Fixture,
    result: &ContextResult,
    revision: &RecordId,
    original: &str,
    request: &ContextRequest,
) {
    assert!(!result.network_used);
    assert!(result.selection_packet().is_none());
    assert!(
        result.passages().len() <= 4,
        "original per-owner passage cap"
    );
    for passage in result.passages() {
        assert_eq!(passage.locator.path, fixture.payload(revision));
        assert_eq!(
            passage.locator.observed_hash,
            Blake3Hash::digest(original.as_bytes())
        );
        assert_eq!(passage.eligibility, Eligibility::Current);
        assert_eq!(passage.label, ExcerptLabel::CapturedSource);
        assert_eq!(passage.span.slice(original).unwrap(), passage.text);
        assert!(passage.text.len() <= request.documents.limits.excerpt_bytes);
        assert!(!passage.citations.is_empty());
        for citation in &passage.citations {
            let CitationRef::Source(reference) = citation else {
                panic!("direct exact SourceRef required")
            };
            assert_eq!(reference.source_id, fixture.source);
            assert_eq!(&reference.source_revision, revision);
            assert_eq!(reference.span, passage.span);
            assert_eq!(
                reference.quote_hash,
                Blake3Hash::digest(passage.text.as_bytes())
            );
            assert!(
                result
                    .text()
                    .contains(&serde_json::to_string(citation).unwrap())
            );
        }
        assert!(result.text().contains(&passage.text));
    }
    assert_eq!(result.usage().rendered_bytes, result.text().len());
    assert!(result.usage().rendered_bytes <= request.budget.max_bytes);
    assert!(result.usage().estimated_tokens <= request.budget.max_tokens);
    assert!(result.usage().verification_bytes <= request.verification_budget.max_bytes);
    assert!(result.usage().verification_files <= request.verification_budget.max_files);
    assert!(result.usage().verification_entries <= request.verification_budget.max_entries);
    assert!(matches!(
        result.verification(),
        SnapshotVerification::IndexedEvidence {
            global_membership_verified: false,
            ..
        }
    ));
}
fn assert_complete(result: &ContextResult, wait: &str) {
    // Any exact complete packet is allowed: adjacent units may coalesce or remain separate.
    let returned = result
        .passages()
        .iter()
        .map(|p| p.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        returned.contains("# Restart manual"),
        "original ancestor required as quoted bytes"
    );
    assert!(
        returned.contains("## Authorized restart"),
        "scope is evidence, not navigation metadata"
    );
    assert!(
        returned.contains(CONDITION),
        "do not lose the original only/negative qualifications"
    );
    assert!(
        returned.contains(&CHECKLIST.replace("18 minutes", wait)),
        "retain the whole list including wait, units and evidence conditions"
    );
}

#[test]
fn automatic_group_context_saved_page_refresh_withdraw_and_rebuild_preserve_exact_history() {
    let fixture = Fixture::new();
    let original = body("18 minutes");
    assert!(CONDITION.len() < 1024 && CHECKLIST.len() < 1024);
    assert!(
        CONDITION.len() + CHECKLIST.len() > 1024,
        "fixture requires jointly admitted separate original units"
    );
    let first = fixture.context();
    authenticate(
        &fixture,
        &first,
        &fixture.first_revision,
        &original,
        &fixture.request(),
    );
    assert_complete(&first, "18 minutes");
    let create = fixture.page_request(&first, &fixture.first_revision, None);
    fixture.app.page_batch(create).unwrap();
    let page_path = fixture.root.join("pages/restart.md");
    let first_page = fs::read(&page_path).unwrap();
    assert!(
        std::str::from_utf8(&first_page)
            .unwrap()
            .contains(first.text())
    );
    let first_page_hash = Blake3Hash::digest(&first_page);
    let stale = fixture.page_request(
        &first,
        &fixture.first_revision,
        Some(first_page_hash.clone()),
    );

    let newer = body("23 minutes");
    let refreshed = fixture
        .app
        .source_refresh(fixture.source.clone(), capture(&newer))
        .unwrap();
    assert_eq!(refreshed.status, Some(ChangeStatus::Committed));
    let current_revision = refreshed.allocated_ids["revision"].clone();
    assert_ne!(current_revision, fixture.first_revision);
    assert_eq!(
        fixture.app.page_batch(stale).unwrap_err().code,
        ErrorCode::ContentConflict
    );
    assert_eq!(
        fs::read(&page_path).unwrap(),
        first_page,
        "refused stale proposal must not alter Page"
    );
    let current = fixture.context();
    authenticate(
        &fixture,
        &current,
        &current_revision,
        &newer,
        &fixture.request(),
    );
    assert_complete(&current, "23 minutes");
    assert!(!current.text().contains("18 minutes"));
    fixture
        .app
        .page_batch(fixture.page_request(
            &current,
            &current_revision,
            Some(first_page_hash.clone()),
        ))
        .unwrap();
    let updated_page = fs::read(&page_path).unwrap();
    assert_ne!(updated_page, first_page);
    assert!(
        std::str::from_utf8(&updated_page)
            .unwrap()
            .contains(current.text())
    );
    // Reusing an obsolete author guard cannot overwrite the updated Page.
    assert_eq!(
        fixture
            .app
            .page_batch(fixture.page_request(&current, &current_revision, Some(first_page_hash)))
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
    let old_payload = fixture
        .root
        .join(fixture.payload(&fixture.first_revision).as_str());
    let new_payload = fixture
        .root
        .join(fixture.payload(&current_revision).as_str());
    assert_eq!(fs::read(&old_payload).unwrap(), original.as_bytes());
    assert_eq!(fs::read(&new_payload).unwrap(), newer.as_bytes());
    // Old citations still authenticate immutable bytes, without claiming current eligibility.
    for passage in first.passages() {
        assert_eq!(
            passage
                .span
                .slice(std::str::from_utf8(&fs::read(&old_payload).unwrap()).unwrap())
                .unwrap(),
            passage.text
        );
    }
    fs::remove_dir_all(fixture.root.join(".wiki/cache")).unwrap();
    fixture.app.index_rebuild_normalized().unwrap();
    let rebuilt = fixture.context();
    authenticate(
        &fixture,
        &rebuilt,
        &current_revision,
        &newer,
        &fixture.request(),
    );
    assert_complete(&rebuilt, "23 minutes");
    assert_eq!(fs::read(&page_path).unwrap(), updated_page);
    fixture
        .app
        .source_withdraw(fixture.source.clone(), "Workshop procedure withdrawn")
        .unwrap();
    assert!(
        fixture.context().passages().is_empty(),
        "ordinary current context excludes withdrawn source"
    );
    fs::remove_dir_all(fixture.root.join(".wiki/cache")).unwrap();
    fixture.app.index_rebuild_normalized().unwrap();
    assert!(fixture.context().passages().is_empty());
    assert_eq!(fs::read(&old_payload).unwrap(), original.as_bytes());
    assert_eq!(fs::read(&new_payload).unwrap(), newer.as_bytes());
    assert_eq!(
        fs::read(page_path).unwrap(),
        updated_page,
        "withdrawal does not rewrite saved quoted history"
    );
}

struct ChangeSourceHead {
    target: PathBuf,
    bytes: Vec<u8>,
}
impl ContextFault for ChangeSourceHead {
    fn check(&self, checkpoint: ContextCheckpoint) -> Result<()> {
        assert_eq!(
            checkpoint,
            ContextCheckpoint::BeforeFinalVerification { attempt: 0 }
        );
        fs::write(&self.target, &self.bytes).unwrap();
        Ok(())
    }
}
#[test]
fn automatic_group_budget_rejection_is_joint_and_final_source_head_recheck_is_required() {
    let fixture = Fixture::new();
    let original = body("18 minutes");
    let mut tight = fixture.request();
    tight.budget.max_bytes = 900;
    tight.budget.max_tokens = 225;
    let (limited, trace) = context::with_candidate_ordering_trace(|| {
        indexed_documents::context(&fixture.catalog, QUERY, &tight, &ContextOptions::default())
    });
    let trials = trace
        .iter()
        .find(|stage| stage["stage"] == "packing_trials")
        .expect("actual integrated packing trials")["rows"]
        .as_array()
        .unwrap();
    let joint = trials
        .iter()
        .filter(|trial| {
            trial["identity"]["proposals"]
                .as_array()
                .is_some_and(|p| p.len() > 1)
        })
        .collect::<Vec<_>>();
    assert!(
        !joint.is_empty(),
        "automatic coordinator must try the structural group"
    );
    for trial in joint {
        assert!(!trial["cap_reason"].is_null());
        assert_ne!(
            trial["outcome"], "accepted",
            "no partial group admission under a failing joint trial"
        );
    }
    // Original candidates remain legitimate fallbacks, so an empty/error or bounded exact subset is allowed.
    match limited {
        Ok(result) => {
            authenticate(
                &fixture,
                &result,
                &fixture.first_revision,
                &original,
                &tight,
            );
            assert!(!(result.text().contains(CONDITION) && result.text().contains(CHECKLIST)));
            assert!(result.truncated() || !result.omissions().is_empty());
        }
        Err(error) => assert_eq!(error.code, ErrorCode::BudgetExceeded),
    }
    let mut proof_bound = fixture.request();
    proof_bound.verification_budget.max_bytes = 1;
    assert_eq!(
        indexed_documents::context(
            &fixture.catalog,
            QUERY,
            &proof_bound,
            &ContextOptions::default()
        )
        .unwrap_err()
        .code,
        ErrorCode::BudgetExceeded
    );
    let target = fixture.root.join(fixture.source_path().as_str());
    let mut changed_head = fs::read(&target).unwrap();
    changed_head.extend_from_slice(b"\nExternal edit after selected authentication.\n");
    assert_eq!(
        indexed_documents::context(
            &fixture.catalog,
            QUERY,
            &fixture.request(),
            &ContextOptions {
                fault: Some(Arc::new(ChangeSourceHead {
                    target,
                    bytes: changed_head
                })),
                ..Default::default()
            }
        )
        .unwrap_err()
        .code,
        ErrorCode::FreshnessConflict
    );
    assert_eq!(
        fs::read(
            fixture
                .root
                .join(fixture.payload(&fixture.first_revision).as_str())
        )
        .unwrap(),
        original.as_bytes()
    );
}

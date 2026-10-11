//! Exact literal discovery enters the existing closed selected document proof.
use super::{
    ContextBudget, ContextCheckpoint, ContextFault, ContextOptions, ContextRequest, ContextScope,
    ExcerptLabel, QueryPlan, SearchMode, VerificationBudget, context,
    context_selection_packet::{SelectionAction, SelectionReply},
    indexed_documents, lexical, selected_documents, selected_search,
};
use crate::{
    catalog::{Catalog, SnapshotVerification, query_types::QueryReadLimits},
    changes::ChangeDraft,
    domain::*,
    records::parse_note,
    sources::{CaptureRequest, ExtractionInput, SourceOrigin, SourceStore},
    vault::{VaultFs, VaultRoot, WriterPermit},
};
use std::{fs, path::PathBuf, sync::Arc, time::Duration};

const QUERY: &str = "東京::Vec<T>";
const BODY: &str = "Café 🦀 東京::Vec<T> needs the violet permit. OR % _ \\\nSecond exact 東京::Vec<T> occurrence.\n";

fn id(value: &str) -> RecordId {
    RecordId::new(value).unwrap()
}
fn path(value: &str) -> VaultRelativePath {
    VaultRelativePath::new(value).unwrap()
}
fn capture(body: &[u8]) -> CaptureRequest {
    CaptureRequest {
        title: "Exact captured signature".into(),
        origin_kind: SourceOrigin::LocalFile,
        origin: "literal fixture.txt".into(),
        original: body.to_vec(),
        extraction: ExtractionInput::Utf8Preserve,
        media_type: Some("text/plain".into()),
    }
}
fn literal_plan() -> QueryPlan {
    let mut plan = QueryPlan {
        mode: SearchMode::Literal,
        ..Default::default()
    };
    plan.limits.excerpt_bytes = 1024;
    plan
}
struct Fixture {
    _temp: tempfile::TempDir,
    catalog: Catalog,
    source: RecordId,
    revision: RecordId,
    content: VaultRelativePath,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("WIKI.md"), "---\nwiki_schema: '1'\nwiki_id: vault_literal_workflow\nwiki_kind: vault\ntitle: Literal workflow\n---\n").unwrap();
        fs::write(temp.path().join("page.md"), format!("---\nwiki_schema: '1'\nwiki_id: page_literal_workflow\nwiki_kind: page\nwiki_status: reviewed\ntitle: headerliteral\n---\n# Authored signature\n\n{QUERY} stays authored text.\n")).unwrap();
        fs::write(
            temp.path().join("unselected.md"),
            "Unrelated orchard notes.\n",
        )
        .unwrap();
        let handle = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let captured = SourceStore::new(handle.clone())
            .plan_capture(capture(BODY.as_bytes()))
            .unwrap();
        let source = captured.source_id.clone();
        let revision = captured.revision_id.clone();
        let content = path(&format!("sources/{source}/revisions/{revision}/content.md"));
        let fixture = Self {
            _temp: temp,
            catalog: Catalog::new(handle, id("vault_literal_workflow")),
            source,
            revision,
            content,
        };
        fixture.seed(captured.draft.unwrap());
        let writer =
            WriterPermit::acquire(fixture.catalog.fs().root(), Duration::from_secs(1)).unwrap();
        fixture.catalog.rebuild_normalized(&writer).unwrap();
        drop(writer);
        fixture
    }
    fn seed(&self, draft: ChangeDraft) {
        for operation in draft.operations {
            let target = self
                .catalog
                .fs()
                .root()
                .path()
                .join(operation.target.as_str());
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(target, operation.proposed.unwrap()).unwrap();
        }
    }
    fn sync(&self) {
        let writer =
            WriterPermit::acquire(self.catalog.fs().root(), Duration::from_secs(1)).unwrap();
        self.catalog.sync_normalized(&writer).unwrap();
    }
    fn payload_plan(&self) -> QueryPlan {
        let mut plan = literal_plan();
        plan.filters.path_prefix = Some(self.content.as_str().into());
        plan
    }
    fn request(&self) -> ContextRequest {
        ContextRequest {
            scope: ContextScope::IndexedDocuments,
            documents: self.payload_plan(),
            ..Default::default()
        }
    }
    fn full(&self, relative: &VaultRelativePath) -> PathBuf {
        self.catalog.fs().root().path().join(relative.as_str())
    }
}
fn assert_source_quote(fixture: &Fixture, text: &str, citation: &CitationRef) {
    let CitationRef::Source(reference) = citation else {
        panic!("captured source citation required")
    };
    assert_eq!(reference.source_id, fixture.source);
    assert_eq!(reference.source_revision, fixture.revision);
    assert_eq!(reference.span.slice(BODY).unwrap(), text);
    assert_eq!(reference.quote_hash, Blake3Hash::digest(text.as_bytes()));
}

#[test]
fn literal_verified_discovery_and_automatic_context_bind_exact_unicode_source_bytes() {
    let fixture = Fixture::new();
    let reader = fixture
        .catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let cached = lexical::search_catalog(&reader, QUERY, &fixture.payload_plan()).unwrap();
    assert_eq!(cached.hits.len(), 1);
    assert_eq!(cached.hits[0].excerpt.matched_spans.len(), 2);
    assert!(cached.hits[0].excerpt.citation.is_none());
    let verified = selected_search::search(
        &fixture.catalog,
        QUERY,
        &fixture.payload_plan(),
        &VerificationBudget::default(),
    )
    .unwrap();
    let hit = &verified.hits[0];
    assert_eq!(cached.hits[0].locator, hit.locator);
    assert_eq!(cached.hits[0].excerpt.text, hit.excerpt.text);
    assert_eq!(cached.hits[0].excerpt.span, hit.excerpt.span);
    assert_eq!(
        cached.hits[0].excerpt.matched_spans,
        hit.excerpt.matched_spans
    );
    assert_eq!(hit.excerpt.label, ExcerptLabel::CapturedSource);
    assert_source_quote(
        &fixture,
        &hit.excerpt.text,
        hit.excerpt.citation.as_ref().unwrap(),
    );
    for matched in &hit.excerpt.matched_spans {
        assert_eq!(matched.slice(BODY).unwrap(), QUERY);
    }
    assert!(matches!(
        verified.verification,
        SnapshotVerification::IndexedEvidence {
            global_membership_verified: false,
            ..
        }
    ));
    assert!(!verified.network_used);
    drop(reader);
    let request = fixture.request();
    let result = indexed_documents::context(
        &fixture.catalog,
        QUERY,
        &request,
        &ContextOptions::default(),
    )
    .unwrap();
    assert!(!result.passages().is_empty());
    assert!(result.selection_packet().is_none());
    for passage in result.passages() {
        assert_eq!(passage.label, ExcerptLabel::CapturedSource);
        assert_eq!(passage.span.slice(BODY).unwrap(), passage.text);
        assert!(passage.text.contains(QUERY));
        for citation in &passage.citations {
            assert_source_quote(&fixture, &passage.text, citation);
        }
        assert!(!passage.citations.is_empty());
    }
    assert!(
        result.passages().len() <= 2,
        "literal retains the existing legacy passage policy"
    );
    assert_eq!(result.usage().rendered_bytes, result.text().len());
    assert!(result.usage().rendered_bytes <= request.budget.max_bytes);
    assert!(result.usage().estimated_tokens <= request.budget.max_tokens);
    assert!(matches!(
        result.verification(),
        SnapshotVerification::IndexedEvidence {
            global_membership_verified: false,
            ..
        }
    ));
    assert!(!result.network_used);
}

#[test]
fn literal_authored_frontmatter_is_exact_file_text_without_source_citations() {
    let fixture = Fixture::new();
    let mut plan = literal_plan();
    plan.filters.path_prefix = Some("page.md".into());
    let result = selected_search::search(
        &fixture.catalog,
        "headerliteral",
        &plan,
        &VerificationBudget::default(),
    )
    .unwrap();
    let hit = &result.hits[0];
    let bytes = fs::read(fixture.full(&path("page.md"))).unwrap();
    let raw = std::str::from_utf8(&bytes).unwrap();
    let note = parse_note(&bytes);
    assert!(
        !std::str::from_utf8(note.body())
            .unwrap()
            .contains("headerliteral")
    );
    assert_eq!(hit.excerpt.span.slice(raw).unwrap(), hit.excerpt.text);
    assert_eq!(
        hit.excerpt.matched_spans[0].slice(raw).unwrap(),
        "headerliteral"
    );
    assert!(hit.excerpt.matched_spans[0].start() < (bytes.len() - note.body().len()) as u64);
    assert!(hit.excerpt.citation.is_none());
    assert_eq!(hit.excerpt.label, ExcerptLabel::NoteText);
    let request = ContextRequest {
        scope: ContextScope::IndexedDocuments,
        documents: plan,
        ..Default::default()
    };
    let context = indexed_documents::context(
        &fixture.catalog,
        "headerliteral",
        &request,
        &ContextOptions::default(),
    )
    .unwrap();
    assert!(!context.passages().is_empty());
    for passage in context.passages() {
        assert_eq!(passage.span.slice(raw).unwrap(), passage.text);
        assert_eq!(passage.label, ExcerptLabel::NoteText);
        assert!(passage.citations.is_empty());
    }
}

#[test]
fn literal_selected_same_size_same_mtime_tamper_refuses_search_and_context() {
    let fixture = Fixture::new();
    let file = fixture.full(&fixture.content);
    let before = fs::metadata(&file).unwrap();
    let edited = BODY.replace("violet", "orange");
    fs::write(&file, &edited).unwrap();
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(before.modified().unwrap()))
        .unwrap();
    assert_eq!(fs::metadata(&file).unwrap().len(), before.len());
    assert_eq!(
        fs::metadata(&file).unwrap().modified().unwrap(),
        before.modified().unwrap()
    );
    assert_eq!(
        selected_search::search(
            &fixture.catalog,
            QUERY,
            &fixture.payload_plan(),
            &VerificationBudget::default()
        )
        .unwrap_err()
        .code,
        ErrorCode::FreshnessConflict
    );
    assert_eq!(
        indexed_documents::context(
            &fixture.catalog,
            QUERY,
            &fixture.request(),
            &ContextOptions::default()
        )
        .unwrap_err()
        .code,
        ErrorCode::FreshnessConflict
    );
    assert_eq!(fs::read(file).unwrap(), edited.as_bytes());
}

#[test]
fn literal_wrong_owner_revision_and_non_utf8_spans_cannot_escape_closed_binding() {
    let fixture = Fixture::new();
    let reader = fixture
        .catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let hit = lexical::search_catalog(&reader, QUERY, &fixture.payload_plan())
        .unwrap()
        .hits
        .remove(0);
    let proof = selected_documents::authenticate(
        &fixture.catalog,
        &reader,
        &[fixture.content.clone()],
        &VerificationBudget::default(),
    )
    .unwrap();
    for variant in 0..4 {
        let mut bad = hit.clone();
        match variant {
            0 => bad.source_id = Some(id("source_wrong_literal")),
            1 => bad.owner_revision = Some(id("revision_wrong_literal")),
            2 => bad.locator.record.as_mut().unwrap().record_id = id("revision_wrong_literal"),
            _ => {
                let start = BODY.find('é').unwrap() as u64 + 1;
                bad.excerpt.matched_spans = vec![ByteSpan::new(start, start + 1).unwrap()];
            }
        }
        assert_eq!(
            selected_search::bind_hit_for_test(&mut bad, &proof, fixture.catalog.vault_id())
                .unwrap_err()
                .code,
            ErrorCode::FreshnessConflict,
            "variant {variant}"
        );
    }
}

struct EditBeforeEmission {
    path: PathBuf,
    bytes: Vec<u8>,
}
impl ContextFault for EditBeforeEmission {
    fn check(&self, checkpoint: ContextCheckpoint) -> Result<()> {
        assert_eq!(
            checkpoint,
            ContextCheckpoint::BeforeFinalVerification { attempt: 0 }
        );
        fs::write(&self.path, &self.bytes).unwrap();
        Ok(())
    }
}
#[test]
fn literal_final_recheck_rejects_actual_post_authentication_edits() {
    for context_arm in [false, true] {
        let fixture = Fixture::new();
        let edited = BODY.replace("violet", "orange").into_bytes();
        let file = fixture.full(&fixture.content);
        let code = if context_arm {
            indexed_documents::context(
                &fixture.catalog,
                QUERY,
                &fixture.request(),
                &ContextOptions {
                    fault: Some(Arc::new(EditBeforeEmission {
                        path: file.clone(),
                        bytes: edited.clone(),
                    })),
                    ..Default::default()
                },
            )
            .unwrap_err()
            .code
        } else {
            let (result, stats) = selected_search::measured_search(
                &fixture.catalog,
                QUERY,
                &fixture.payload_plan(),
                &VerificationBudget::default(),
                || {
                    fs::write(&file, &edited).unwrap();
                    Ok(())
                },
            );
            assert!(stats.proof_work.unwrap().1 > 0);
            result.unwrap_err().code
        };
        assert_eq!(code, ErrorCode::FreshnessConflict);
        assert_eq!(fs::read(file).unwrap(), edited);
    }
}

#[test]
fn literal_final_recheck_rejects_selected_source_refresh_after_discovery() {
    let fixture = Fixture::new();
    let (result, stats) = selected_search::measured_search(
        &fixture.catalog,
        QUERY,
        &fixture.payload_plan(),
        &VerificationBudget::default(),
        || {
            let refreshed = SourceStore::new(fixture.catalog.fs().clone())
                .plan_refresh(
                    &fixture.source,
                    capture(BODY.replace("violet", "orange").as_bytes()),
                )
                .unwrap();
            assert_ne!(refreshed.revision_id, fixture.revision);
            fixture.seed(refreshed.draft.unwrap());
            fixture.sync();
            Ok(())
        },
    );
    assert!(stats.proof_work.unwrap().1 > 0);
    assert_eq!(result.unwrap_err().code, ErrorCode::FreshnessConflict);
    assert_eq!(
        fs::read(fixture.full(&fixture.content)).unwrap(),
        BODY.as_bytes()
    );
}

#[test]
fn literal_unselected_edit_does_not_expand_selected_proof() {
    let fixture = Fixture::new();
    let before = selected_search::search(
        &fixture.catalog,
        QUERY,
        &fixture.payload_plan(),
        &VerificationBudget::default(),
    )
    .unwrap();
    fs::write(
        fixture.full(&path("unselected.md")),
        "Unindexed unrelated external edit.\n",
    )
    .unwrap();
    let after = selected_search::search(
        &fixture.catalog,
        QUERY,
        &fixture.payload_plan(),
        &VerificationBudget::default(),
    )
    .unwrap();
    assert_eq!(before.hits, after.hits);
    assert_eq!(before.dependency_fingerprint, after.dependency_fingerprint);
    assert!(matches!(
        after.verification,
        SnapshotVerification::IndexedEvidence {
            global_membership_verified: false,
            ..
        }
    ));
}

#[test]
fn literal_refresh_withdraw_and_stale_cursor_preserve_immutable_quotes() {
    let fixture = Fixture::new();
    let mut pagination = literal_plan();
    pagination.limits.hits = 1;
    let first = selected_search::search(
        &fixture.catalog,
        QUERY,
        &pagination,
        &VerificationBudget::default(),
    )
    .unwrap();
    pagination.cursor = Some(
        first
            .next_cursor
            .expect("authored and captured matches give two pages"),
    );
    let newer = BODY.replace("violet", "amber");
    let refresh = SourceStore::new(fixture.catalog.fs().clone())
        .plan_refresh(&fixture.source, capture(newer.as_bytes()))
        .unwrap();
    let new_revision = refresh.revision_id.clone();
    fixture.seed(refresh.draft.unwrap());
    assert_eq!(
        selected_search::search(
            &fixture.catalog,
            QUERY,
            &fixture.payload_plan(),
            &VerificationBudget::default()
        )
        .unwrap_err()
        .code,
        ErrorCode::FreshnessConflict
    );
    fixture.sync();
    assert_eq!(
        selected_search::search(
            &fixture.catalog,
            QUERY,
            &pagination,
            &VerificationBudget::default()
        )
        .unwrap_err()
        .code,
        ErrorCode::CursorStale
    );
    let mut current = literal_plan();
    current.filters.source_ids = vec![fixture.source.clone()];
    let result = selected_search::search(
        &fixture.catalog,
        QUERY,
        &current,
        &VerificationBudget::default(),
    )
    .unwrap();
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].owner_revision, Some(new_revision));
    assert!(result.hits[0].excerpt.text.contains("amber"));
    let withdrawal = SourceStore::new(fixture.catalog.fs().clone())
        .plan_withdraw(&fixture.source, "Literal fixture withdrawal")
        .unwrap();
    fixture.seed(withdrawal.draft.unwrap());
    fixture.sync();
    assert!(
        selected_search::search(
            &fixture.catalog,
            QUERY,
            &current,
            &VerificationBudget::default()
        )
        .unwrap()
        .hits
        .is_empty()
    );
    current.filters.include_historical = true;
    let historical = selected_search::search(
        &fixture.catalog,
        QUERY,
        &current,
        &VerificationBudget::default(),
    )
    .unwrap();
    assert_eq!(historical.hits.len(), 2);
    assert!(
        historical
            .hits
            .iter()
            .all(|hit| hit.eligibility == Eligibility::Withdrawn && hit.excerpt.citation.is_some())
    );
    assert_eq!(
        fs::read(fixture.full(&fixture.content)).unwrap(),
        BODY.as_bytes()
    );
}

#[test]
fn literal_proof_and_render_budgets_never_return_partial_authenticated_success() {
    let fixture = Fixture::new();
    for budget in [
        VerificationBudget {
            max_bytes: 1,
            ..Default::default()
        },
        VerificationBudget {
            max_files: 1,
            ..Default::default()
        },
        VerificationBudget {
            max_entries: 1,
            ..Default::default()
        },
    ] {
        assert_eq!(
            selected_search::search(&fixture.catalog, QUERY, &fixture.payload_plan(), &budget)
                .unwrap_err()
                .code,
            ErrorCode::BudgetExceeded
        );
        let mut request = fixture.request();
        request.verification_budget = budget;
        assert_eq!(
            indexed_documents::context(
                &fixture.catalog,
                QUERY,
                &request,
                &ContextOptions::default()
            )
            .unwrap_err()
            .code,
            ErrorCode::BudgetExceeded
        );
    }
    let mut request = fixture.request();
    request.budget = ContextBudget {
        max_bytes: 1,
        max_tokens: 1,
        ..Default::default()
    };
    assert_eq!(
        indexed_documents::context(
            &fixture.catalog,
            QUERY,
            &request,
            &ContextOptions::default()
        )
        .unwrap_err()
        .code,
        ErrorCode::BudgetExceeded
    );
}

#[test]
fn literal_preview_is_request_only_and_host_or_other_scopes_stay_explicit() {
    let fixture = Fixture::new();
    let preview = selected_search::preview(":: % _ OR", &literal_plan()).unwrap();
    assert_eq!(preview["database_opened"], false);
    assert_eq!(preview["verification_performed"], false);
    assert_eq!(preview["network_used"], false);
    assert!(preview["hits"].is_null());
    assert_eq!(preview["citations"], serde_json::json!([]));
    assert_eq!(
        selected_search::preview("\0", &literal_plan())
            .unwrap_err()
            .code,
        ErrorCode::Usage
    );
    for selection in [
        SelectionAction::Prepare,
        SelectionAction::Apply(SelectionReply {
            packet_fingerprint: Blake3Hash::digest(b"unknown literal packet"),
            ordered_ids: vec![],
        }),
    ] {
        assert_eq!(
            indexed_documents::context(
                &fixture.catalog,
                QUERY,
                &fixture.request(),
                &ContextOptions {
                    selection,
                    ..Default::default()
                }
            )
            .unwrap_err()
            .code,
            ErrorCode::Usage
        );
    }
    for scope in [
        ContextScope::Current,
        ContextScope::Historical,
        ContextScope::Snapshot,
        ContextScope::IndexedEvidence,
    ] {
        let mut request = fixture.request();
        request.scope = scope;
        assert!(
            indexed_documents::context(
                &fixture.catalog,
                QUERY,
                &request,
                &ContextOptions::default()
            )
            .is_err(),
            "scope {scope:?}"
        );
    }
    let mut request = fixture.request();
    request.graph = Some(Default::default());
    assert_eq!(
        context::validate_request(QUERY, &request).unwrap_err().code,
        ErrorCode::Usage
    );
}

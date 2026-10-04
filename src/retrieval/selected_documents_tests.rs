use super::*;
use crate::{
    catalog::query_types::QueryReadLimits,
    vault::{VaultFs, VaultRoot, WriterPermit},
};
use std::{fs, time::Duration};

fn path(p: &str) -> VaultRelativePath {
    VaultRelativePath::new(p).unwrap()
}
fn fixture() -> (tempfile::TempDir, Catalog) {
    fixture_with_missing_original(false)
}
fn fixture_with_missing_original(missing_original: bool) -> (tempfile::TempDir, Catalog) {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("WIKI.md"), "---\nwiki_schema: '1'\nwiki_id: vault_selected_documents\nwiki_kind: vault\ntitle: Selected proof\n---\n").unwrap();
    fs::write(temp.path().join("page.md"), "---\nwiki_schema: '1'\nwiki_id: page_selected\nwiki_kind: page\ntitle: Selected page\nwiki_status: reviewed\nwiki_depends_on_ids: [assertion_support]\n---\n# Authored café\n\nSelected exact prose.\n").unwrap();
    fs::write(temp.path().join("entity.md"), "---\nwiki_schema: '1'\nwiki_id: entity_fixture\nwiki_kind: entity\ntitle: Fixture\nwiki_status: active\nwiki_entity_type: component\n---\n").unwrap();
    fs::write(temp.path().join("support.md"), "---\nwiki_schema: '1'\nwiki_id: assertion_support\nwiki_kind: assertion\ntitle: Support\nwiki_status: accepted\nwiki_subject_id: entity_fixture\nwiki_object_id: entity_fixture\nwiki_predicate: uses\n---\nSupport exact prose.\n").unwrap();
    fs::write(temp.path().join("decision.md"), "---\nwiki_schema: '1'\nwiki_id: decision_support\nwiki_kind: decision\ntitle: Support decision\nwiki_status: active\nwiki_action: accept\nwiki_created_at: '2026-10-03T00:00:00Z'\nwiki_input_ids: [assertion_support]\nwiki_output_ids: [assertion_support]\n---\n").unwrap();
    fs::write(
        temp.path().join("plain.md"),
        "# Ordinary\n\nUnadopted exact prose.\n",
    )
    .unwrap();
    fs::write(temp.path().join("malformed.md"), "---\nwiki_schema: 'unsupported'\nwiki_id: page_malformed\nwiki_kind: page\ntitle: Malformed\n---\nReadable malformed prose.\n").unwrap();
    let handle = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
    let content = b"Support exact captured prose.";
    let plan = crate::sources::SourceStore::new(handle.clone())
        .plan_capture(crate::sources::CaptureRequest {
            title: "Captured proof".into(),
            origin_kind: crate::sources::SourceOrigin::LocalFile,
            origin: "fixture.txt".into(),
            original: content.to_vec(),
            extraction: crate::sources::ExtractionInput::Utf8Preserve,
            media_type: None,
        })
        .unwrap();
    let source_id = plan.source_id.clone();
    let revision_id = plan.revision_id.clone();
    for operation in plan.draft.unwrap().operations {
        let target = temp.path().join(operation.target.as_str());
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        if !(missing_original && operation.target.as_str().ends_with("original.bin")) {
            fs::write(target, operation.proposed.unwrap()).unwrap();
        }
    }
    let mut evidence = format!("---\nwiki_schema: '1'\nwiki_id: evidence_support\nwiki_kind: evidence\ntitle: Support evidence\nwiki_status: active\nwiki_assertion_id: assertion_support\nwiki_source_id: '{source_id}'\nwiki_source_revision: '{revision_id}'\nwiki_stance: supports\nwiki_locator_kind: utf8-bytes\nwiki_span_start: 0\nwiki_span_end: {}\nwiki_quote_hash: {}\n---\n", content.len(), Blake3Hash::digest(content)).into_bytes();
    evidence.extend(crate::sources::evidence::exact_quote_body(content, "\n", "Support").unwrap());
    fs::write(temp.path().join("evidence.md"), evidence).unwrap();
    // A broken secondary support member must not poison intact current support.
    let mut invalid_evidence = "---\nwiki_schema: '1'\nwiki_id: evidence_broken\nwiki_kind: evidence\ntitle: Broken secondary evidence\nwiki_status: active\nwiki_assertion_id: assertion_support\nwiki_source_id: source_missing\nwiki_source_revision: revision_missing\nwiki_stance: supports\nwiki_locator_kind: utf8-bytes\nwiki_span_start: 0\nwiki_span_end: 3\nwiki_quote_hash: ".to_owned();
    invalid_evidence.push_str(Blake3Hash::digest(b"bad").as_str());
    invalid_evidence.push_str("\n---\n");
    let mut invalid_evidence = invalid_evidence.into_bytes();
    invalid_evidence
        .extend(crate::sources::evidence::exact_quote_body(b"bad", "\n", "Broken").unwrap());
    fs::write(temp.path().join("broken-evidence.md"), invalid_evidence).unwrap();

    let vault = RecordId::new("vault_selected_documents").unwrap();
    let writer = WriterPermit::acquire(handle.root(), Duration::from_secs(1)).unwrap();
    let catalog = Catalog::new(handle, vault);
    catalog.rebuild_normalized(&writer).unwrap();
    drop(writer);
    (temp, catalog)
}
#[test]
fn selected_authored_closure_authenticates_and_rechecks() {
    let (_temp, catalog) = fixture();
    let reader = catalog.query_snapshot(QueryReadLimits::default()).unwrap();
    let mut proof = authenticate(
        &catalog,
        &reader,
        &[path("page.md")],
        &VerificationBudget::default(),
    )
    .unwrap();
    assert!(
        proof
            .records
            .contains_key(&RecordId::new("assertion_support").unwrap())
    );
    assert_eq!(
        proof.documents[&path("page.md")].eligibility,
        Eligibility::Current
    );
    assert_eq!(
        proof.records[&RecordId::new("evidence_broken").unwrap()].eligibility,
        Eligibility::Invalid
    );
    proof.recheck(&catalog, &reader).unwrap();
}
#[test]
fn selected_support_same_size_edit_is_rejected_and_unrelated_edit_is_bounded() {
    let (temp, catalog) = fixture();
    let reader = catalog.query_snapshot(QueryReadLimits::default()).unwrap();
    let mut proof = authenticate(
        &catalog,
        &reader,
        &[path("page.md")],
        &VerificationBudget::default(),
    )
    .unwrap();
    fs::write(temp.path().join("unrelated.md"), "Unindexed unrelated text").unwrap();
    proof.recheck(&catalog, &reader).unwrap();
    let raw = fs::read_to_string(temp.path().join("support.md")).unwrap();
    fs::write(
        temp.path().join("support.md"),
        raw.replace("Support exact prose.", "Support other prose."),
    )
    .unwrap();
    assert_eq!(
        proof.recheck(&catalog, &reader).unwrap_err().code,
        ErrorCode::FreshnessConflict
    );
    assert!(
        matches!(authenticate(&catalog,&reader,&[path("page.md")],&VerificationBudget::default()),Err(e) if e.code==ErrorCode::FreshnessConflict)
    );
}
#[test]
fn unadopted_and_malformed_text_can_be_read_without_eligible_claim() {
    let (_temp, catalog) = fixture();
    let reader = catalog.query_snapshot(QueryReadLimits::default()).unwrap();
    let proof = authenticate(
        &catalog,
        &reader,
        &[path("plain.md"), path("malformed.md")],
        &VerificationBudget::default(),
    )
    .unwrap();
    assert_eq!(
        proof.documents[&path("plain.md")].reasons,
        vec!["note_text"]
    );
    assert_eq!(
        proof.documents[&path("malformed.md")].eligibility,
        Eligibility::Invalid
    );
    assert!(
        proof.documents[&path("malformed.md")]
            .raw_text
            .contains("Readable malformed prose.")
    );
}
#[test]
fn marker_is_always_bound_even_for_empty_selection() {
    let (temp, catalog) = fixture();
    let reader = catalog.query_snapshot(QueryReadLimits::default()).unwrap();
    let mut proof = authenticate(&catalog, &reader, &[], &VerificationBudget::default()).unwrap();
    fs::write(temp.path().join("WIKI.md"), "Changed vault marker").unwrap();
    assert_eq!(
        proof.recheck(&catalog, &reader).unwrap_err().code,
        ErrorCode::FreshnessConflict
    );
}
#[test]
fn proof_budget_is_shared_with_final_recheck() {
    let (_temp, catalog) = fixture();
    let reader = catalog.query_snapshot(QueryReadLimits::default()).unwrap();
    let budget = VerificationBudget {
        max_files: 3,
        ..Default::default()
    };
    let mut proof = authenticate(&catalog, &reader, &[path("plain.md")], &budget).unwrap();
    assert_eq!(
        proof.recheck(&catalog, &reader).unwrap_err().code,
        ErrorCode::BudgetExceeded
    );
}

#[test]
fn decision_without_prior_structural_effect_is_still_verified() {
    let (temp, catalog) = fixture();
    let reader = catalog.query_snapshot(QueryReadLimits::default()).unwrap();
    let mut proof = authenticate(
        &catalog,
        &reader,
        &[path("page.md")],
        &VerificationBudget::default(),
    )
    .unwrap();
    assert!(
        proof
            .records
            .contains_key(&RecordId::new("decision_support").unwrap())
    );
    let raw = fs::read_to_string(temp.path().join("decision.md")).unwrap();
    fs::write(
        temp.path().join("decision.md"),
        raw.replace("wiki_action: accept", "wiki_action: reject"),
    )
    .unwrap();
    assert_eq!(
        proof.recheck(&catalog, &reader).unwrap_err().code,
        ErrorCode::FreshnessConflict
    );
}
#[test]
fn expected_absent_original_becoming_present_rejects_final_proof() {
    let (temp, catalog) = fixture_with_missing_original(true);
    let reader = catalog.query_snapshot(QueryReadLimits::default()).unwrap();
    let mut proof = authenticate(
        &catalog,
        &reader,
        &[path("page.md")],
        &VerificationBudget::default(),
    )
    .unwrap();
    let absent = proof
        .states
        .iter()
        .find(|(path, state)| {
            path.as_str().ends_with("original.bin") && **state == ExpectedState::Absent
        })
        .unwrap()
        .0
        .clone();
    fs::write(
        temp.path().join(absent.as_str()),
        b"Support exact captured prose.",
    )
    .unwrap();
    assert_eq!(
        proof.recheck(&catalog, &reader).unwrap_err().code,
        ErrorCode::FreshnessConflict
    );
}
#[test]
fn selected_content_disappearing_after_authentication_rejects_final_proof() {
    let (temp, catalog) = fixture();
    let reader = catalog.query_snapshot(QueryReadLimits::default()).unwrap();
    let mut proof = authenticate(
        &catalog,
        &reader,
        &[path("page.md")],
        &VerificationBudget::default(),
    )
    .unwrap();
    fs::remove_file(temp.path().join("page.md")).unwrap();
    assert_eq!(
        proof.recheck(&catalog, &reader).unwrap_err().code,
        ErrorCode::FreshnessConflict
    );
}

#[test]
fn historical_capture_read_preserves_bytes_and_historical_eligibility() {
    let (temp, catalog) = fixture();
    let reader = catalog.query_snapshot(QueryReadLimits::default()).unwrap();
    let proof = authenticate(
        &catalog,
        &reader,
        &[path("page.md")],
        &VerificationBudget::default(),
    )
    .unwrap();
    let source = proof
        .records
        .values()
        .find(|r| r.record.kind() == RecordKind::Source)
        .unwrap();
    let revision = proof
        .records
        .values()
        .find(|r| r.record.kind() == RecordKind::Revision)
        .unwrap();
    let historical_path = payload(revision, "wiki_content_path").unwrap();
    let source_id = source.record.id().clone();
    drop(proof);
    drop(reader);
    let plan = crate::sources::SourceStore::new(catalog.fs().clone())
        .plan_refresh(
            &source_id,
            crate::sources::CaptureRequest {
                title: "Refreshed captured proof".into(),
                origin_kind: crate::sources::SourceOrigin::LocalFile,
                origin: "fixture.txt".into(),
                original: b"New current captured prose.".to_vec(),
                extraction: crate::sources::ExtractionInput::Utf8Preserve,
                media_type: None,
            },
        )
        .unwrap();
    for operation in plan.draft.unwrap().operations {
        let target = temp.path().join(operation.target.as_str());
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, operation.proposed.unwrap()).unwrap();
    }
    let writer = WriterPermit::acquire(catalog.fs().root(), Duration::from_secs(1)).unwrap();
    catalog.sync_normalized(&writer).unwrap();
    drop(writer);
    let reader = catalog.query_snapshot(QueryReadLimits::default()).unwrap();
    let mut historical = authenticate(
        &catalog,
        &reader,
        &[historical_path.clone()],
        &VerificationBudget::default(),
    )
    .unwrap();
    assert_eq!(
        historical.documents[&historical_path].eligibility,
        Eligibility::Historical
    );
    assert_eq!(
        historical.documents[&historical_path].raw_text,
        "Support exact captured prose."
    );
    historical.recheck(&catalog, &reader).unwrap();
}

#[test]
fn missing_proof_rows_and_forged_selected_document_metadata_refuse() {
    for sql in [
        "DELETE FROM record_eligibility_facts WHERE record_id='page_selected'",
        "DELETE FROM record_direct_paths WHERE path LIKE '%/original.bin'",
        "DELETE FROM semantic_edges WHERE owner_id='page_selected' AND target_id='assertion_support'",
        "UPDATE documents SET title='Forged cached title' WHERE path='page.md'",
    ] {
        let (_temp, catalog) = fixture();
        let reader = catalog.query_snapshot(QueryReadLimits::default()).unwrap();
        let database = reader.connection().path().unwrap().to_owned();
        drop(reader);
        // Deliberate test-only corruption. Keep the connection alive so WAL
        // cleanup does not replace the intended row-corruption failure.
        let corrupt = rusqlite::Connection::open(database).unwrap();
        assert!(corrupt.execute(sql, []).unwrap() > 0, "{sql}");
        let reader = catalog.query_snapshot(QueryReadLimits::default()).unwrap();
        let error = match authenticate(
            &catalog,
            &reader,
            &[path("page.md")],
            &VerificationBudget::default(),
        ) {
            Ok(_) => panic!("selected corruption accepted: {sql}"),
            Err(error) => error,
        };
        assert!(
            matches!(
                error.code,
                ErrorCode::IndexCorrupt | ErrorCode::FreshnessConflict
            ),
            "{sql}: {error:?}"
        );
    }
}

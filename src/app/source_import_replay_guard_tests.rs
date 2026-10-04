//! Narrow adversarial guards for the authenticated proofless-abort route.
//! Native fixtures and receipts; no process-death or power-loss claim.
use super::*;
use crate::changes::{
    ChangeEvent, ChangeManifest, ConflictResolutionMode, journal, operation_authority, outcome,
};

fn staged(migrated: bool) -> (Fixture, PreparedChange) {
    let fixture = Fixture::new(migrated, 1);
    fixture.prepare();
    let (change, captures) = prepared_import(&fixture, Cut::BeforeProof);
    assert_eq!(captures.len(), 1);
    assert!(
        fixture
            .physical(&proof_path(&change))
            .symlink_metadata()
            .is_err()
    );
    assert!(
        fixture
            .physical(&rel(format!(
                "changes/{}/indexed-delta.json",
                change.change_id
            )))
            .is_file()
    );
    (fixture, change)
}
fn proof_path(change: &PreparedChange) -> VaultRelativePath {
    rel(format!("changes/{}/validation.json", change.change_id))
}
fn manifest(fixture: &Fixture, change: &PreparedChange) -> ChangeManifest {
    let (manifest, hash) = fixture
        .app()
        .engine()
        .unwrap()
        .load_manifest_structure(&change.change_id)
        .unwrap();
    assert_eq!(hash, change.manifest_hash);
    manifest
}
fn authenticated_abort(fixture: &Fixture, change: &PreparedChange) {
    let app = fixture.app();
    let writer = app.writer().unwrap();
    let report = app.engine().unwrap().abort(&writer, change).unwrap();
    assert_eq!(report.status, ChangeStatus::Aborted);
    assert!(report.snapshot.is_none());
    assert_eq!(
        outcome::terminal_ever_applying(
            app.fs(),
            &manifest(fixture, change),
            &change.manifest_hash
        )
        .unwrap(),
        Some(false)
    );
}
fn route(
    fixture: &Fixture,
    change: &PreparedChange,
) -> crate::domain::Result<Option<crate::changes::indexed_refresh::IndexedRefreshProof>> {
    let app = fixture.app();
    let authority = app.catalog().operation_state()?;
    app.engine()?
        .indexed_replay_proof(&manifest(fixture, change), change, authority.as_ref())
}
fn refuses_unchanged(fixture: &Fixture, change: &PreparedChange) {
    let before = preservation_snapshot(fixture.temp.path());
    assert!(route(fixture, change).is_err());
    assert!(
        fixture
            .app()
            .changes_apply(change.change_id.clone())
            .is_err()
    );
    assert_eq!(preservation_snapshot(fixture.temp.path()), before);
}

#[test]
fn retained_delta_missing_validation_requires_authenticated_never_applying_abort() {
    for migrated in [false, true] {
        let (fixture, change) = staged(migrated);
        let error = route(&fixture, &change).unwrap_err();
        assert!(error.message.contains("lost its retained validation proof"));
        refuses_unchanged(&fixture, &change);

        let committed = Fixture::new(migrated, 1);
        committed.prepare();
        let result = committed
            .app()
            .source_import_run(&committed.manifest, KEY, 1, 1)
            .unwrap();
        let change = result.last_group.unwrap().change;
        let report = outcome::terminal_report(
            committed.app().fs(),
            &manifest(&committed, &change),
            &change.manifest_hash,
        )
        .unwrap()
        .unwrap();
        assert_eq!(report.status, ChangeStatus::Committed);
        assert!(report.snapshot.is_some());
        fs::remove_file(committed.physical(&proof_path(&change))).unwrap();
        refuses_unchanged(&committed, &change);
    }
}

#[test]
fn authenticated_abort_accepts_allowed_journal_prefixes_only_with_normalized_authority() {
    for migrated in [false, true] {
        let (fixture, change) = staged(migrated);
        authenticated_abort(&fixture, &change);
        let app = fixture.app();
        assert!(
            app.engine()
                .unwrap()
                .indexed_replay_proof(&manifest(&fixture, &change), &change, None)
                .is_err()
        );
        let path = fixture.physical(&journal::journal_path(&change.change_id).unwrap());
        let full = fs::read(&path).unwrap();
        let state =
            journal::decode_journal(&full, &manifest(&fixture, &change), &change.manifest_hash)
                .unwrap();
        assert_eq!(state.frames.len(), 2);
        let prepared_prefix = journal::encode_frame(&state.frames[0]).unwrap();
        for bytes in [full, prepared_prefix, vec![]] {
            fs::write(&path, bytes).unwrap();
            let before = preservation_snapshot(fixture.temp.path());
            assert!(route(&fixture, &change).unwrap().is_none());
            assert_eq!(preservation_snapshot(fixture.temp.path()), before);
            let historical = fixture
                .app()
                .changes_apply(change.change_id.clone())
                .unwrap();
            assert_eq!(historical.status, Some(ChangeStatus::Aborted));
            assert!(historical.snapshot.is_none());
            assert!(historical.reused);
            assert_eq!(preservation_snapshot(fixture.temp.path()), before);
        }
        fs::remove_file(&path).unwrap();
        assert!(route(&fixture, &change).unwrap().is_none());
        assert_eq!(
            fixture
                .app()
                .changes_apply(change.change_id)
                .unwrap()
                .status,
            Some(ChangeStatus::Aborted)
        );
    }
}

#[test]
fn present_invalid_validation_and_corrupt_or_divergent_terminal_evidence_refuse() {
    use std::os::unix::fs::symlink;
    for migrated in [false, true] {
        let (fixture, change) = staged(migrated);
        authenticated_abort(&fixture, &change);
        let path = fixture.physical(&proof_path(&change));
        for bytes in [
            b"{malformed".as_slice(),
            br#"{"proof":{"version":99}}"#.as_slice(),
        ] {
            fs::write(&path, bytes).unwrap();
            refuses_unchanged(&fixture, &change);
            fs::remove_file(&path).unwrap();
        }
        fs::create_dir(&path).unwrap();
        refuses_unchanged(&fixture, &change);
        fs::remove_dir(&path).unwrap();
        let outside = fixture.temp.path().join("unknown-proof.json");
        fs::write(&outside, br#"{"proof":{"version":99}}"#).unwrap();
        for target in [
            outside.clone(),
            fixture.temp.path().join("missing-proof.json"),
        ] {
            symlink(&target, &path).unwrap();
            refuses_unchanged(&fixture, &change);
            fs::remove_file(&path).unwrap();
        }
        // This checks invalid PRESENT bytes even through a hardlink. It makes
        // no assertion that every older read path universally rejects nlink>1.
        fs::hard_link(&outside, &path).unwrap();
        refuses_unchanged(&fixture, &change);
        fs::remove_file(&path).unwrap();

        let receipt = fixture.physical(&rel(format!("changes/{}/outcome.json", change.change_id)));
        let original = fs::read(&receipt).unwrap();
        let mut corrupted: Value = serde_json::from_slice(&original).unwrap();
        corrupted["checksum"] = json!(Blake3Hash::digest(b"wrong receipt checksum"));
        fs::write(&receipt, serde_json::to_vec(&corrupted).unwrap()).unwrap();
        refuses_unchanged(&fixture, &change);
        fs::write(&receipt, &original).unwrap();
        fs::remove_file(&receipt).unwrap();
        refuses_unchanged(&fixture, &change);
        fs::write(&receipt, original).unwrap();

        let journal_path = fixture.physical(&journal::journal_path(&change.change_id).unwrap());
        let original_journal = fs::read(&journal_path).unwrap();
        let mut frames = journal::decode_journal(
            &original_journal,
            &manifest(&fixture, &change),
            &change.manifest_hash,
        )
        .unwrap()
        .frames;
        frames[1].event = ChangeEvent::Applying;
        let divergent = frames
            .iter()
            .flat_map(|frame| journal::encode_frame(frame).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            journal::decode_journal(
                &divergent,
                &manifest(&fixture, &change),
                &change.manifest_hash
            )
            .unwrap()
            .status,
            Some(ChangeStatus::Applying)
        );
        fs::write(&journal_path, divergent).unwrap();
        refuses_unchanged(&fixture, &change);
        fs::write(&journal_path, original_journal).unwrap();
        assert!(route(&fixture, &change).unwrap().is_none());
    }
}

#[test]
fn authenticated_ever_applying_abort_refuses_even_with_live_prepared_prefix() {
    for migrated in [false, true] {
        let (fixture, change) = staged(migrated);
        let app = fixture.app();
        let engine = app.engine().unwrap();
        let manifest = manifest(&fixture, &change);
        let journal_path = fixture.physical(&journal::journal_path(&change.change_id).unwrap());
        let prefix = fs::read(&journal_path).unwrap();
        let writer = app.writer().unwrap();
        // Adversarial internal transition, with actual framed events and an
        // engine-retained authenticated receipt. No canonical target is written.
        journal::append_event(
            app.fs(),
            &writer,
            &manifest,
            &change.manifest_hash,
            ChangeEvent::Applying,
        )
        .unwrap();
        let observations = engine.observe(&manifest).unwrap();
        assert!(observations.iter().all(|seen| seen.observed == seen.before));
        journal::append_event(
            app.fs(),
            &writer,
            &manifest,
            &change.manifest_hash,
            ChangeEvent::Conflict {
                phase: "test prewrite cut".into(),
                observations,
            },
        )
        .unwrap();
        let request = engine
            .resolution_plan(&change, ConflictResolutionMode::Abandon)
            .unwrap();
        let state = journal::append_event(
            app.fs(),
            &writer,
            &manifest,
            &change.manifest_hash,
            ChangeEvent::ResolutionAccepted {
                mode: request.mode,
                conflict_sequence: request.conflict_sequence,
                conflict_hash: request.conflict_hash,
                observations: request.observations,
            },
        )
        .unwrap();
        let report =
            outcome::retain_terminal(&engine, &writer, &manifest, &change.manifest_hash, &state)
                .unwrap();
        assert_eq!(report.status, ChangeStatus::Aborted);
        drop(writer);
        assert_eq!(
            outcome::terminal_ever_applying(app.fs(), &manifest, &change.manifest_hash).unwrap(),
            Some(true)
        );
        fs::write(&journal_path, prefix).unwrap();
        assert_eq!(
            journal::load_journal(app.fs(), &manifest, &change.manifest_hash)
                .unwrap()
                .status,
            Some(ChangeStatus::Prepared)
        );
        assert_eq!(
            outcome::terminal_ever_applying(app.fs(), &manifest, &change.manifest_hash).unwrap(),
            Some(true)
        );
        refuses_unchanged(&fixture, &change);
    }
}

#[test]
fn historical_abort_refuses_same_id_active_and_preserves_unrelated_active_operation() {
    for migrated in [false, true] {
        let (fixture, change) = staged(migrated);
        authenticated_abort(&fixture, &change);
        let app = fixture.app();
        let authority_path = fixture.root.join(".wiki/state/operations.json");
        let idle_bytes = fs::read(&authority_path).unwrap();
        for active_change in [
            change.clone(),
            PreparedChange {
                change_id: change.change_id.clone(),
                manifest_hash: Blake3Hash::digest(b"different active manifest"),
            },
        ] {
            let writer = app.writer().unwrap();
            let authority = app.catalog().operation_state().unwrap().unwrap();
            let mut intended = authority.publication().clone();
            intended.epoch += 1;
            operation_authority::begin(app.fs(), &writer, &authority, active_change, intended)
                .unwrap();
            drop(writer);
            refuses_unchanged(&fixture, &change);
            // Reset only the disposable adversarial case to its exact saved
            // idle authority; this is not a recovery or cancellation claim.
            fs::write(&authority_path, &idle_bytes).unwrap();
        }
        let writer = app.writer().unwrap();
        let authority = app.catalog().operation_state().unwrap().unwrap();
        let unrelated = PreparedChange {
            change_id: RecordId::new("change_unrelated_guard").unwrap(),
            manifest_hash: Blake3Hash::digest(b"unrelated binding"),
        };
        let mut intended = authority.publication().clone();
        intended.epoch += 1;
        let active =
            operation_authority::begin(app.fs(), &writer, &authority, unrelated.clone(), intended)
                .unwrap();
        drop(writer);
        let before = preservation_snapshot(fixture.temp.path());
        assert!(route(&fixture, &change).unwrap().is_none());
        let historical = app.changes_apply(change.change_id.clone()).unwrap();
        assert_eq!(historical.status, Some(ChangeStatus::Aborted));
        assert!(historical.reused);
        let after = app.catalog().operation_state().unwrap().unwrap();
        assert!(after.same_revision(&active));
        assert_eq!(after.active().unwrap().change, unrelated);
        assert_eq!(preservation_snapshot(fixture.temp.path()), before);
    }
}

#[test]
fn historical_abort_refuses_receipt_sync_error_or_change_during_sync() {
    for migrated in [false, true] {
        for cut in [
            Cut::OutcomeReceiptSyncError,
            Cut::OutcomeReceiptChangedDuringSync,
        ] {
            for recover in [false, true] {
                let (fixture, change) = staged(migrated);
                authenticated_abort(&fixture, &change);
                let receipt =
                    fixture.physical(&rel(format!("changes/{}/outcome.json", change.change_id)));
                let relative = receipt
                    .strip_prefix(fs::canonicalize(fixture.temp.path()).unwrap())
                    .unwrap()
                    .to_owned();
                let mut before = preservation_snapshot(fixture.temp.path());
                before.remove(&relative);
                let adapter = Arc::new(ImportIo::new(Some(cut), vec![receipt]));
                let app = OfflineApp::new(
                    VaultFs::with_io(VaultRoot::explicit(&fixture.root).unwrap(), adapter.clone()),
                    options(),
                )
                .unwrap();
                let failed = if recover {
                    app.recover().is_err()
                } else {
                    app.changes_apply(change.change_id).is_err()
                };
                assert!(failed, "{migrated}/{cut:?}/recover={recover}");
                assert!(adapter.fired.load(Ordering::SeqCst));
                let mut after = preservation_snapshot(fixture.temp.path());
                after.remove(&relative); // Only the explicitly injected receipt may change.
                assert_eq!(after, before);
            }
        }
    }
}

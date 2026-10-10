//! Terminal-only legacy Page handoff; inherited disposable normalized fixtures.
use super::*;
use crate::changes::indexed_refresh::IndexedRefreshProof;
#[cfg(unix)]
use std::sync::atomic::{AtomicI32, AtomicU8};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

fn archive(change: &PreparedChange) -> VaultRelativePath {
    rel(format!(
        "changes/{}/legacy-page-validation-v4.json",
        change.change_id
    ))
}
fn baseline(change: &PreparedChange) -> VaultRelativePath {
    crate::changes::indexed_refresh::baseline_path(change).unwrap()
}
fn outcome_file(change: &PreparedChange) -> VaultRelativePath {
    rel(format!("changes/{}/outcome.json", change.change_id))
}
fn raw_proof(f: &Fixture, change: &PreparedChange) -> IndexedRefreshProof {
    let value: serde_json::Value =
        serde_json::from_slice(&fs::read(f.physical(&baseline(change))).unwrap()).unwrap();
    serde_json::from_value(value["proof"].clone()).unwrap()
}
#[derive(serde::Serialize)]
struct ViewReceipt<'a> {
    proof: &'a IndexedRefreshProof,
    checksum: Blake3Hash,
}

fn write_view(f: &Fixture, change: &PreparedChange, proof: &IndexedRefreshProof) {
    // Match production typed field order so negative fixtures exercise their
    // targeted authority defect, rather than incidental JSON-map ordering.
    let receipt = ViewReceipt {
        proof,
        checksum: Blake3Hash::digest(serde_json::to_vec(proof).unwrap()),
    };
    fs::write(
        f.physical(&baseline(change)),
        serde_json::to_vec(&receipt).unwrap(),
    )
    .unwrap();
}

#[derive(serde::Serialize, serde::Deserialize)]
struct AdversarialTerminalProof {
    version: u32,
    change: PreparedChange,
    status: ChangeStatus,
    snapshot: Option<crate::domain::ReadSnapshot>,
    baseline_note_hash: Blake3Hash,
    finalized_note_hash: Blake3Hash,
    journal: Vec<crate::changes::JournalFrame>,
}

// Hold the actual new finalizer at its first archive rename, after original
// canonical commit/outcome/ack. This is a genuine old full-v4 terminal state.
fn committed_full_v4(f: &Fixture) -> (ChangeInspection, Vec<u8>) {
    let (parent, _) = f.marker_change();
    let legacy = old_public_path_inverse(f, &parent);
    let plain = f.app();
    let io = Arc::new(LegacyAdmissionIo {
        target: f.physical(&archive(&legacy.prepared)),
        after: false,
        reached: AtomicBool::new(false),
        rendezvous: None,
    });
    let faulted = OfflineApp::new(
        VaultFs::with_io(plain.fs().root().clone(), io.clone()),
        options(),
    )
    .unwrap();
    assert!(
        faulted
            .changes_apply(legacy.prepared.change_id.clone())
            .is_err()
    );
    assert!(
        io.reached.load(Ordering::SeqCst),
        "actual archive-before finalizer cut reached"
    );
    f.assert_page(&f.original);
    let details = f
        .app()
        .changes_show(legacy.prepared.change_id.clone())
        .unwrap();
    assert_eq!(details.status, ChangeStatus::Committed);
    assert_eq!(indexed_and_committed(&details), (1, 1));
    assert!(
        f.app()
            .catalog()
            .operation_state()
            .unwrap()
            .unwrap()
            .active()
            .is_none()
    );
    let raw = fs::read(f.physical(&baseline(&legacy.prepared))).unwrap();
    assert_eq!(raw_proof(f, &legacy.prepared).version, 4);
    assert!(!f.physical(&archive(&legacy.prepared)).exists());
    (legacy, raw)
}

pub(super) fn assert_view(
    f: &Fixture,
    change: &PreparedChange,
    full: &[u8],
) -> IndexedRefreshProof {
    assert_eq!(fs::read(f.physical(&archive(change))).unwrap(), full);
    let view = raw_proof(f, change);
    assert_eq!(view.version, 3);
    assert_eq!(view.change, *change);
    let engine = ChangeEngine::new(f.app().fs().clone()).unwrap();
    let resolved = engine.load_indexed_refresh_proof(change).unwrap().unwrap();
    assert_eq!(resolved.version, 4);
    assert_eq!(resolved.intended, view.intended);
    assert_ne!(
        crate::catalog::source_refresh::intended(&view.base, change, &view.delta_hash, 3).unwrap(),
        view.intended
    );
    let original: serde_json::Value = serde_json::from_slice(full).unwrap();
    let original: IndexedRefreshProof = serde_json::from_value(original["proof"].clone()).unwrap();
    assert_eq!(resolved, original);
    let marker = Blake3Hash::digest(
        serde_json::to_vec(&(
            "lwiki.legacy-page-terminal-view.v1",
            Blake3Hash::digest(full),
        ))
        .unwrap(),
    );
    assert_eq!(view.delta_hash, marker);
    assert!(
        !f.physical(&crate::catalog::source_refresh::delta_path(change).unwrap())
            .exists()
    );
    view
}

#[test]
fn fresh_legacy_apply_and_old_full_v4_retry_finalize_exact_envelope_without_republication() {
    for retained in [false, true] {
        for historical in [false, true] {
            let f = Fixture::new(retained);
            let (legacy, full) = if historical {
                committed_full_v4(&f)
            } else {
                let (parent, _) = f.marker_change();
                let legacy = old_public_path_inverse(&f, &parent);
                retain_legacy_admission(&f, &legacy.prepared);
                let full = fs::read(f.physical(&baseline(&legacy.prepared))).unwrap();
                (legacy, full)
            };
            let publication = f.publication();
            let manifest_before = fs::read(f.physical(
                &crate::changes::prepare::manifest_path(&legacy.prepared.change_id).unwrap(),
            ))
            .unwrap();
            let result = f
                .app()
                .changes_apply(legacy.prepared.change_id.clone())
                .unwrap();
            assert_eq!(result.change.as_ref(), Some(&legacy.prepared));
            assert_eq!(result.status, Some(ChangeStatus::Committed));
            assert_eq!(result.reused, historical);
            assert_eq!(f.publication().1, publication.1 + u64::from(!historical));
            let view = assert_view(&f, &legacy.prepared, &full);
            let details = f
                .app()
                .changes_show(legacy.prepared.change_id.clone())
                .unwrap();
            assert_eq!(details.manifest, legacy.manifest);
            assert_eq!(indexed_and_committed(&details), (1, 1));
            if historical {
                assert_eq!(
                    fs::read(
                        f.physical(
                            &crate::changes::prepare::manifest_path(&legacy.prepared.change_id)
                                .unwrap()
                        )
                    )
                    .unwrap(),
                    manifest_before
                );
            }
            for dry_run in [true, false, true] {
                let before = tree(&f.root, !dry_run);
                let retry = f
                    .with_options(OperationOptions {
                        dry_run,
                        ..options()
                    })
                    .changes_apply(legacy.prepared.change_id.clone())
                    .unwrap();
                assert!(retry.reused);
                assert_eq!(retry.change.as_ref(), Some(&legacy.prepared));
                assert_eq!(retry.snapshot.as_ref(), Some(&view.intended));
                assert_tree_unchanged(&f.root, !dry_run, &before);
            }
            f.assert_page(&f.original);
            f.assert_source_history();
        }
    }
}

#[test]
fn terminal_view_survives_later_author_edits_superseded_publication_and_expired_payloads() {
    for retained in [false, true] {
        let f = Fixture::new(retained);
        let (legacy, full) = committed_full_v4(&f);
        f.app()
            .changes_apply(legacy.prepared.change_id.clone())
            .unwrap();
        let view = assert_view(&f, &legacy.prepared, &full);
        let mut later = f.original.clone();
        later.extend_from_slice(UNFAMILIAR.as_bytes());
        fs::write(f.physical(&rel(PAGE)), &later).unwrap();
        f.app().page_put(rel("pages/terminal-later.md"),
            b"---\nwiki_schema: '1'\nwiki_kind: page\nwiki_id: page_terminal_later\ntitle: Later page\nwiki_status: reviewed\n---\nLater publication.\n".to_vec(), None).unwrap();
        fs::write(f.physical(&rel(PAGE)), &later).unwrap();
        let publication = f.publication();
        assert!(publication.1 > view.intended.generation);
        for op in &legacy.manifest.operations {
            for payload in [&op.before_payload, &op.after_payload]
                .into_iter()
                .flatten()
            {
                let p = f.physical(&payload.path);
                if p.exists() {
                    fs::remove_file(p).unwrap();
                }
            }
        }
        for dry_run in [true, false] {
            let before = tree(&f.root, !dry_run);
            let retry = f
                .with_options(OperationOptions {
                    dry_run,
                    ..options()
                })
                .changes_apply(legacy.prepared.change_id.clone())
                .unwrap();
            assert_eq!(retry.snapshot.as_ref(), Some(&view.intended));
            assert!(retry.reused);
            assert_tree_unchanged(&f.root, !dry_run, &before);
        }
        assert_eq!(f.bytes(), later);
        assert_eq!(f.publication(), publication);
        assert_eq!(
            fs::read(f.physical(&archive(&legacy.prepared))).unwrap(),
            full
        );
        f.assert_source_history();
    }
}

fn assert_refusal(f: &Fixture, change: &PreparedChange) {
    let author = f.bytes();
    // Some adversarial authority states intentionally have no valid floor, so
    // compare all exact file evidence rather than ask it for a publication.
    for dry_run in [true, false] {
        let before = tree(&f.root, !dry_run);
        assert!(
            f.with_options(OperationOptions {
                dry_run,
                ..options()
            })
            .changes_apply(change.change_id.clone())
            .is_err()
        );
        assert_tree_unchanged(&f.root, !dry_run, &before);
        assert_eq!(f.bytes(), author);
    }
    f.assert_source_history();
}

#[test]
fn terminal_view_refuses_missing_corrupt_foreign_and_rechecksummed_archive_or_view_authority() {
    for retained in [false, true] {
        for case in [
            "missing",
            "corrupt",
            "foreign-change",
            "foreign-vault",
            "foreign-base",
            "foreign-intended",
            "foreign-operation",
            "marker",
            "projection",
            "outcome-missing",
            "outcome-mismatch",
            "unacknowledged",
            "active-same",
            "separate-delta",
            "oversize",
        ] {
            let f = Fixture::new(retained);
            let (legacy, full) = committed_full_v4(&f);
            // The archived exact envelope must also permit idempotent completion
            // if the first replace survived while validation remained full-v4.
            fs::write(f.physical(&archive(&legacy.prepared)), &full).unwrap();
            f.app()
                .changes_apply(legacy.prepared.change_id.clone())
                .unwrap();
            let mut proof = assert_view(&f, &legacy.prepared, &full);
            let archive_path = f.physical(&archive(&legacy.prepared));
            match case {
                "missing" => fs::remove_file(&archive_path).unwrap(),
                "corrupt" => fs::write(&archive_path, b"{corrupt").unwrap(),
                "oversize" => fs::write(
                    &archive_path,
                    vec![b'x'; crate::changes::indexed_refresh::MAX_LEGACY_PAGE_ENVELOPE_BYTES + 1],
                )
                .unwrap(),
                "marker" => {
                    proof.delta_hash = Blake3Hash::digest(b"wrong recomputed-checksum marker");
                    write_view(&f, &legacy.prepared, &proof);
                }
                "projection" => {
                    let guard = crate::changes::ReadDependency {
                        path: rel("WIKI.md"),
                        expected: crate::vault::ExpectedState::Hash(Blake3Hash::digest(
                            fs::read(f.physical(&rel("WIKI.md"))).unwrap(),
                        )),
                    };
                    proof.before.push(guard.clone());
                    proof.after.push(guard);
                    proof.before.sort_by(|a, b| a.path.cmp(&b.path));
                    proof.after.sort_by(|a, b| a.path.cmp(&b.path));
                    write_view(&f, &legacy.prepared, &proof);
                }
                "outcome-missing" => {
                    fs::remove_file(f.physical(&outcome_file(&legacy.prepared))).unwrap()
                }
                "outcome-mismatch" => {
                    let p = f.physical(&outcome_file(&legacy.prepared));
                    let mut receipt: serde_json::Value =
                        serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
                    receipt["proof"]["snapshot"]["generation"] =
                        serde_json::json!(proof.intended.generation + 1);
                    // Explicit adversarial checksum-correct foreign terminal data.
                    let typed: AdversarialTerminalProof =
                        serde_json::from_value(receipt["proof"].clone()).unwrap();
                    receipt["checksum"] =
                        serde_json::json!(Blake3Hash::digest(serde_json::to_vec(&typed).unwrap()));
                    fs::write(p, serde_json::to_vec(&receipt).unwrap()).unwrap();
                }
                "unacknowledged" => {
                    let p = f.root.join(".wiki/state/operations.json");
                    let mut state: serde_json::Value =
                        serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
                    state["publication"]["epoch"] = serde_json::json!(proof.base.generation);
                    fs::write(p, serde_json::to_vec(&state).unwrap()).unwrap();
                }
                "active-same" => {
                    use crate::changes::operation_authority::{self as ops, Presence, Publication};
                    let app = f.app();
                    let engine = ChangeEngine::new(app.fs().clone()).unwrap();
                    let authority = ops::load(app.fs(), engine.vault_id(), Presence::Required)
                        .unwrap()
                        .unwrap();
                    let writer = WriterPermit::acquire(app.fs().root(), Duration::ZERO).unwrap();
                    ops::begin(
                        app.fs(),
                        &writer,
                        &authority,
                        legacy.prepared.clone(),
                        Publication {
                            file_id: authority.publication().file_id.clone(),
                            epoch: authority.publication().epoch + 1,
                        },
                    )
                    .unwrap();
                }
                "separate-delta" => fs::write(
                    f.physical(
                        &crate::catalog::source_refresh::delta_path(&legacy.prepared).unwrap(),
                    ),
                    b"{}",
                )
                .unwrap(),
                _ => {
                    // Mutate the archived full-v4 proof and re-encode its typed
                    // normal checksum, then rebind the view marker to those bytes.
                    // These are attacker fixtures, never producer compatibility.
                    let mut receipt: serde_json::Value = serde_json::from_slice(&full).unwrap();
                    let mut changed: IndexedRefreshProof =
                        serde_json::from_value(receipt["proof"].clone()).unwrap();
                    match case {
                        "foreign-change" => {
                            changed.change.change_id =
                                crate::domain::RecordId::generate(crate::domain::RecordKind::Change)
                                    .unwrap()
                        }
                        "foreign-vault" => {
                            changed.vault_id =
                                crate::domain::RecordId::generate(crate::domain::RecordKind::Vault)
                                    .unwrap()
                        }
                        "foreign-base" => changed.base.generation += 1,
                        "foreign-intended" => changed.intended.generation += 1,
                        "foreign-operation" => {
                            if let Some(
                                crate::changes::indexed_refresh::IndexedWriteOperation::PageBatch {
                                    pages,
                                },
                            ) = &mut changed.operation
                            {
                                pages[0].id = crate::domain::RecordId::generate(
                                    crate::domain::RecordKind::Page,
                                )
                                .unwrap();
                            } else {
                                unreachable!();
                            }
                        }
                        _ => unreachable!(),
                    }
                    receipt["proof"] = serde_json::to_value(&changed).unwrap();
                    // Rust struct encoding rather than map key order determines
                    // the full receipt checksum. The unchanged embedded delta
                    // remains validly encoded but cannot authenticate changed proof.
                    let delta: crate::catalog::source_refresh::RetainedDelta =
                        serde_json::from_value(receipt["embedded_delta"].clone()).unwrap();
                    receipt["checksum"] = serde_json::json!(Blake3Hash::digest(
                        serde_json::to_vec(&(&changed, Some(&delta))).unwrap()
                    ));
                    let bytes = serde_json::to_vec(&receipt).unwrap();
                    fs::write(&archive_path, &bytes).unwrap();
                    proof.delta_hash = Blake3Hash::digest(
                        serde_json::to_vec(&(
                            "lwiki.legacy-page-terminal-view.v1",
                            Blake3Hash::digest(&bytes),
                        ))
                        .unwrap(),
                    );
                    write_view(&f, &legacy.prepared, &proof);
                }
            }
            assert_refusal(&f, &legacy.prepared);
        }
    }
}

#[test]
fn preterminal_precreated_archive_cannot_commit_or_grant_live_authority() {
    for retained in [false, true] {
        for exact in [false, true] {
            let f = Fixture::new(retained);
            let (parent, marked) = f.marker_change();
            let legacy = old_public_path_inverse(&f, &parent);
            retain_legacy_admission(&f, &legacy.prepared);
            let full = fs::read(f.physical(&baseline(&legacy.prepared))).unwrap();
            fs::write(
                f.physical(&archive(&legacy.prepared)),
                if exact {
                    full.as_slice()
                } else {
                    b"conflicting archive"
                },
            )
            .unwrap();
            let publication = f.publication();
            assert_refusal(&f, &legacy.prepared);
            assert_eq!(f.bytes(), marked);
            assert_eq!(f.publication(), publication);
            let engine = ChangeEngine::new(f.app().fs().clone()).unwrap();
            let state = crate::changes::journal::load_journal(
                engine.fs(),
                &legacy.manifest,
                &legacy.prepared.manifest_hash,
            )
            .unwrap();
            assert_eq!(state.frames, legacy.journal.frames);
        }
    }
}

#[cfg(unix)]
#[test]
fn terminal_archive_symlink_is_refused_without_touching_its_external_target() {
    for retained in [false, true] {
        let f = Fixture::new(retained);
        let (legacy, full) = committed_full_v4(&f);
        f.app()
            .changes_apply(legacy.prepared.change_id.clone())
            .unwrap();
        let p = f.physical(&archive(&legacy.prepared));
        fs::remove_file(&p).unwrap();
        let external = f._temp.path().join("external archive authority.json");
        fs::write(&external, &full).unwrap();
        std::os::unix::fs::symlink(&external, &p).unwrap();
        let author = f.bytes();
        let publication = f.publication();
        for dry_run in [true, false] {
            assert!(
                f.with_options(OperationOptions {
                    dry_run,
                    ..options()
                })
                .changes_apply(legacy.prepared.change_id.clone())
                .is_err()
            );
            assert_eq!(fs::read(&external).unwrap(), full);
            assert_eq!(f.bytes(), author);
            assert_eq!(f.publication(), publication);
        }
        f.assert_source_history();
    }
}

#[cfg(unix)]
const CUTS: &[&str] = &[
    "archive-create-before",
    "archive-create-after",
    "archive-file-sync-before",
    "archive-file-sync-after",
    "archive-replace-before",
    "archive-replace-after",
    "archive-directory-sync-before",
    "archive-directory-sync-after",
    "view-replace-before",
    "view-replace-after",
    "view-directory-sync-before",
    "view-directory-sync-after",
];

#[cfg(unix)]
struct TerminalIo {
    archive: PathBuf,
    view: PathBuf,
    cut: String,
    reached: AtomicBool,
    stage_fd: AtomicI32,
    replaced: AtomicU8,
    rendezvous: Option<Arc<LegacyAdmissionRendezvous>>,
}
#[cfg(unix)]
impl TerminalIo {
    fn reach(&self, name: &str) -> std::io::Result<()> {
        if self.cut == name && !self.reached.swap(true, Ordering::SeqCst) {
            if let Some(marker) = &self.rendezvous {
                marker.pause();
            }
            return Err(std::io::Error::other(format!(
                "reached terminal handoff cut {name}"
            )));
        }
        Ok(())
    }
}
#[cfg(unix)]
impl crate::vault::DurableIo for TerminalIo {
    fn create_stage(&self, p: &Path) -> std::io::Result<fs::File> {
        use std::os::fd::AsRawFd;
        let first_archive =
            p.parent() == self.archive.parent() && self.stage_fd.load(Ordering::SeqCst) == -1;
        if first_archive {
            self.reach("archive-create-before")?;
        }
        let file = crate::vault::NativeIo.create_stage(p)?;
        if first_archive {
            self.stage_fd.store(file.as_raw_fd(), Ordering::SeqCst);
            self.reach("archive-create-after")?;
        }
        Ok(file)
    }
    fn create_private_stage(&self, p: &Path) -> std::io::Result<fs::File> {
        crate::vault::NativeIo.create_private_stage(p)
    }
    fn create_private_directory(&self, p: &Path) -> std::io::Result<()> {
        crate::vault::NativeIo.create_private_directory(p)
    }
    fn open_append(&self, p: &Path) -> std::io::Result<fs::File> {
        crate::vault::NativeIo.open_append(p)
    }
    fn truncate_file(&self, f: &fs::File, n: u64) -> std::io::Result<()> {
        crate::vault::NativeIo.truncate_file(f, n)
    }
    fn write_stage(&self, f: &mut fs::File, b: &[u8]) -> std::io::Result<()> {
        crate::vault::NativeIo.write_stage(f, b)
    }
    fn sync_file(&self, f: &fs::File) -> std::io::Result<()> {
        use std::os::fd::AsRawFd;
        let archive_stage = f.as_raw_fd() == self.stage_fd.load(Ordering::SeqCst)
            && self.replaced.load(Ordering::SeqCst) == 0;
        if archive_stage {
            self.reach("archive-file-sync-before")?;
        }
        crate::vault::NativeIo.sync_file(f)?;
        if archive_stage {
            self.reach("archive-file-sync-after")?;
        }
        Ok(())
    }
    fn replace(&self, staged: &Path, target: &Path) -> std::io::Result<()> {
        let phase = if target == self.archive {
            1
        } else if target == self.view {
            2
        } else {
            0
        };
        if phase == 1 {
            self.reach("archive-replace-before")?;
        }
        if phase == 2 {
            self.reach("view-replace-before")?;
        }
        crate::vault::NativeIo.replace(staged, target)?;
        if phase != 0 {
            self.replaced.store(phase, Ordering::SeqCst);
        }
        if phase == 1 {
            self.reach("archive-replace-after")?;
        }
        if phase == 2 {
            self.reach("view-replace-after")?;
        }
        Ok(())
    }
    fn remove(&self, p: &Path) -> std::io::Result<()> {
        crate::vault::NativeIo.remove(p)
    }
    fn remove_directory(&self, p: &Path) -> std::io::Result<()> {
        crate::vault::NativeIo.remove_directory(p)
    }
    fn create_directory(&self, p: &Path) -> std::io::Result<()> {
        crate::vault::NativeIo.create_directory(p)
    }
    fn sync_directory(&self, p: &Path) -> std::io::Result<crate::vault::DirectorySync> {
        let phase = if p == self.archive.parent().unwrap() {
            self.replaced.swap(0, Ordering::SeqCst)
        } else {
            0
        };
        if phase == 1 {
            self.reach("archive-directory-sync-before")?;
        }
        if phase == 2 {
            self.reach("view-directory-sync-before")?;
        }
        let result = crate::vault::NativeIo.sync_directory(p)?;
        if phase == 1 {
            self.reach("archive-directory-sync-after")?;
        }
        if phase == 2 {
            self.reach("view-directory-sync-after")?;
        }
        Ok(result)
    }
}

#[cfg(unix)]
fn fault_io(
    f: &Fixture,
    change: &PreparedChange,
    cut: &str,
    rendezvous: Option<Arc<LegacyAdmissionRendezvous>>,
) -> Arc<TerminalIo> {
    Arc::new(TerminalIo {
        archive: f.physical(&archive(change)),
        view: f.physical(&baseline(change)),
        cut: cut.into(),
        reached: AtomicBool::new(false),
        stage_fd: AtomicI32::new(-1),
        replaced: AtomicU8::new(0),
        rendezvous,
    })
}

#[cfg(unix)]
#[test]
fn reached_terminal_archive_and_view_io_errors_resume_exact_id_without_canonical_replay() {
    for retained in [false, true] {
        for &cut in CUTS {
            let f = Fixture::new(retained);
            let (legacy, full) = committed_full_v4(&f);
            let publication = f.publication();
            let before = f
                .app()
                .changes_show(legacy.prepared.change_id.clone())
                .unwrap();
            let io = fault_io(&f, &legacy.prepared, cut, None);
            let app = OfflineApp::new(
                VaultFs::with_io(f.app().fs().root().clone(), io.clone()),
                options(),
            )
            .unwrap();
            assert!(
                app.changes_apply(legacy.prepared.change_id.clone())
                    .is_err()
            );
            assert!(
                io.reached.load(Ordering::SeqCst),
                "actual returned-error cut {cut} retained={retained}"
            );
            assert_eq!(f.bytes(), f.original);
            assert_eq!(f.publication(), publication);
            let after = f
                .app()
                .changes_show(legacy.prepared.change_id.clone())
                .unwrap();
            assert_eq!(after.frames, before.frames);
            assert_eq!(after.manifest, before.manifest);
            let retry = f
                .app()
                .changes_apply(legacy.prepared.change_id.clone())
                .unwrap();
            assert!(retry.reused);
            assert_view(&f, &legacy.prepared, &full);
            assert_eq!(f.publication(), publication);
            assert_eq!(
                indexed_and_committed(
                    &f.app()
                        .changes_show(legacy.prepared.change_id.clone())
                        .unwrap()
                ),
                (1, 1)
            );
            f.assert_source_history();
        }
    }
}

#[cfg(unix)]
#[test]
#[ignore = "owned by enabled terminal-view reached SIGKILL wrapper"]
fn terminal_view_native_crash_child() {
    let root = VaultRoot::explicit(std::env::var_os("LWIKI_TERMINAL_VIEW_ROOT").unwrap()).unwrap();
    let change: PreparedChange =
        serde_json::from_str(&std::env::var("LWIKI_TERMINAL_VIEW_CHANGE").unwrap()).unwrap();
    let cut = std::env::var("LWIKI_TERMINAL_VIEW_CUT").unwrap();
    assert!(CUTS.contains(&cut.as_str()));
    let retained = std::env::var("LWIKI_TERMINAL_VIEW_LAYOUT").unwrap() == "retained";
    assert_eq!(crate::storage::layout::active(&root).unwrap(), retained);
    let plain = VaultFs::new(root.clone());
    let marker = Arc::new(LegacyAdmissionRendezvous {
        path: PathBuf::from(std::env::var_os("LWIKI_TERMINAL_VIEW_MARKER").unwrap()),
        bytes: legacy_admission_marker(&cut, retained, &change),
    });
    let io = Arc::new(TerminalIo {
        archive: root.resolve(&archive(&change)).unwrap(),
        view: root.resolve(&baseline(&change)).unwrap(),
        cut: cut.clone(),
        reached: AtomicBool::new(false),
        stage_fd: AtomicI32::new(-1),
        replaced: AtomicU8::new(0),
        rendezvous: Some(marker),
    });
    let app = OfflineApp::new(VaultFs::with_io(plain.root().clone(), io), options()).unwrap();
    app.changes_apply(change.change_id).unwrap();
    panic!("child did not reach exact terminal cut {cut}");
}

#[cfg(unix)]
#[test]
fn native_terminal_handoff_sigkill_reaps_and_resumes_same_id_at_twelve_cuts_both_layouts() {
    use std::{
        os::unix::process::ExitStatusExt,
        process::{Command, Stdio},
        time::Instant,
    };
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            if self.0.try_wait().ok().flatten().is_none() {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }
    for retained in [false, true] {
        for &cut in CUTS {
            let f = Fixture::new(retained);
            let (legacy, full) = committed_full_v4(&f);
            let publication = f.publication();
            let details = f
                .app()
                .changes_show(legacy.prepared.change_id.clone())
                .unwrap();
            let marker = f._temp.path().join("terminal-view-reached029.json");
            let expected = legacy_admission_marker(cut, retained, &legacy.prepared);
            let child_name = format!(
                "{}::terminal_view_native_crash_child",
                module_path!().split_once("::").unwrap().1
            );
            let mut child = Child(
                Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", &child_name, "--ignored", "--nocapture"])
                    .env("LWIKI_TERMINAL_VIEW_ROOT", &f.root)
                    .env(
                        "LWIKI_TERMINAL_VIEW_CHANGE",
                        serde_json::to_string(&legacy.prepared).unwrap(),
                    )
                    .env("LWIKI_TERMINAL_VIEW_CUT", cut)
                    .env(
                        "LWIKI_TERMINAL_VIEW_LAYOUT",
                        if retained { "retained" } else { "original" },
                    )
                    .env("LWIKI_TERMINAL_VIEW_MARKER", &marker)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap(),
            );
            let deadline = Instant::now() + Duration::from_secs(15);
            while !fs::read(&marker).is_ok_and(|bytes| bytes == expected) {
                assert!(
                    child.0.try_wait().unwrap().is_none(),
                    "child exited before exact reached {cut}"
                );
                assert!(
                    Instant::now() < deadline,
                    "reached cut timed out {cut} retained={retained}"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(fs::read(&marker).unwrap(), expected);
            child.0.kill().unwrap();
            assert_eq!(child.0.wait().unwrap().signal(), Some(9));
            assert_eq!(f.bytes(), f.original);
            assert_eq!(f.publication(), publication);
            let after = f
                .app()
                .changes_show(legacy.prepared.change_id.clone())
                .unwrap();
            assert_eq!(after.frames, details.frames);
            assert_eq!(after.manifest, details.manifest);
            let current = fs::read(f.physical(&baseline(&legacy.prepared))).unwrap();
            if current != full {
                assert_view(&f, &legacy.prepared, &full);
            } else if f.physical(&archive(&legacy.prepared)).exists() {
                assert_eq!(
                    fs::read(f.physical(&archive(&legacy.prepared))).unwrap(),
                    full
                );
            }
            // Native wait/reap precedes lock acquisition and restart on the
            // same fixture/ID. No fresh preparation stands in for recovery.
            drop(WriterPermit::acquire(f.app().fs().root(), Duration::from_secs(1)).unwrap());
            let recovery = f.app().recover().unwrap().report.unwrap();
            let recovered = recovery
                .changes
                .iter()
                .find(|x| x.change == legacy.prepared)
                .expect("public recovery must report this exact already-committed handoff");
            assert_eq!(recovered.status, ChangeStatus::Committed);
            // Assert recovery itself completes the handoff. The later exact-ID
            // call tests only idempotence and cannot mask missing recovery.
            let recovered_view = assert_view(&f, &legacy.prepared, &full);
            assert_eq!(recovered.snapshot.as_ref(), Some(&recovered_view.intended));
            let stable = tree(&f.root, true);
            let result = f
                .app()
                .changes_apply(legacy.prepared.change_id.clone())
                .unwrap();
            assert!(result.reused);
            assert_tree_unchanged(&f.root, true, &stable);
            assert_view(&f, &legacy.prepared, &full);
            assert_eq!(f.publication(), publication);
            assert_eq!(
                indexed_and_committed(
                    &f.app()
                        .changes_show(legacy.prepared.change_id.clone())
                        .unwrap()
                ),
                (1, 1)
            );
            f.assert_source_history();
            eprintln!(
                "terminal-view SIGKILL cut={cut} retained={retained} change={} reached=true signal=9 reaped=true recovered=true",
                legacy.prepared.change_id
            );
        }
    }
}

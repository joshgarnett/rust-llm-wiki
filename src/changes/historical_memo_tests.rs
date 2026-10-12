//! Independent adversarial checks of the legacy historical-manifest memo.
//! Filesystem observations come from the vault reader, separately from memo hooks.
use super::*;
use crate::{
    catalog::query_diagnostics,
    changes::apply::maintenance_guard_tests::{self as hooks, Event},
    domain::ReadSnapshot,
    vault::{VaultFs, VaultRoot},
};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

const CURRENT: &str = "sources/source_current/revisions/revision_current";
const HISTORY: &str = "sources/source_history/revisions/revision_history";
fn relative(s: impl Into<String>) -> VaultRelativePath {
    VaultRelativePath::new(s).unwrap()
}
struct Fixture {
    _temp: tempfile::TempDir,
    engine: ChangeEngine,
    writer: WriterPermit,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("WIKI.md"), b"---\nwiki_schema: '1'\nwiki_id: vault_memo\nwiki_kind: vault\ntitle: Historical memo fixture\n---\n").unwrap();
        let root = VaultRoot::explicit(temp.path()).unwrap();
        let writer = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
        Self {
            _temp: temp,
            engine: ChangeEngine::new(VaultFs::new(root)).unwrap(),
            writer,
        }
    }
    fn root(&self) -> &std::path::Path {
        self.engine.fs.root().path()
    }
    fn prepare(&self, tree: &str, members: usize) -> ChangeInspection {
        self.engine
            .prepare(
                &self.writer,
                ChangeDraft {
                    title: "Historical memo fixture".into(),
                    origin: None,
                    inverse_of: None,
                    allocated_ids: BTreeMap::new(),
                    read_preconditions: vec![],
                    operations: (0..members)
                        .map(|i| ExpectedWrite {
                            target: relative(format!("{tree}/{i}.bin")),
                            expected: ExpectedState::Absent,
                            proposed: Some(format!("member {i}").into_bytes()),
                            apply_after: vec![],
                        })
                        .collect(),
                },
            )
            .unwrap()
    }
    fn note(&self, change: &ChangeInspection) -> PathBuf {
        self.root()
            .join(manifest_path(&change.prepared.change_id).unwrap().as_str())
    }
    fn ownership(&self, change: &ChangeInspection) -> Ownership {
        Ownership {
            version: 1,
            change: change.prepared.clone(),
            trees: tree_inventory(&change.manifest).unwrap(),
            validation_hash: Blake3Hash::digest(b"not used by the competing-owner check"),
        }
    }
    fn check(&self, current: &Ownership, memo: &HistoricalRevisionMemo) -> Result<()> {
        self.engine
            .check_competing_owners_with_memo(current, Some(memo))
    }
    fn same_refusal(&self, current: &Ownership, memo: &HistoricalRevisionMemo) -> WikiError {
        let expected = self.engine.check_competing_owners(current).unwrap_err();
        let actual = self.check(current, memo).unwrap_err();
        assert_eq!(actual.code, expected.code);
        assert_eq!(actual.message, expected.message);
        actual
    }
}
#[derive(Default)]
struct Trace {
    discoveries: Vec<usize>,
    lookups: Vec<(String, Option<bool>)>,
}
fn trace(f: &Fixture) -> (Arc<Mutex<Trace>>, hooks::HookGuard) {
    let trace = Arc::new(Mutex::new(Trace::default()));
    let capture = trace.clone();
    let guard = hooks::install(f.engine.fs.root(), move |event| {
        let mut trace = capture.lock().unwrap();
        match event {
            Event::HistoricalDiscovery(n) => trace.discoveries.push(n),
            Event::HistoricalLookup(id, value) => trace.lookups.push((id.to_string(), value)),
            _ => {}
        }
        Ok(())
    });
    (trace, guard)
}
fn assert_physical_reads(
    f: &Fixture,
    current: &Ownership,
    memo: &HistoricalRevisionMemo,
    history: &ChangeInspection,
    expected: usize,
) {
    query_diagnostics::begin();
    let result = f.check(current, memo);
    let observations = query_diagnostics::end();
    result.unwrap();
    assert!(!observations.trace_overflow);
    let note = f.note(history);
    let reads: Vec<_> = observations
        .reads
        .iter()
        .filter(|read| read.path == note.to_string_lossy())
        .collect();
    assert_eq!(
        reads.len(),
        expected,
        "discovery and semantic-site reads remain separate"
    );
    let bytes = fs::read(note).unwrap().len();
    assert!(
        reads
            .iter()
            .all(|read| read.bytes == bytes && read.layer == "managed-read")
    );
}

#[test]
fn warm_hits_still_discover_and_physically_read_same_byte_replacements() {
    let f = Fixture::new();
    let historical = f.prepare(HISTORY, 1);
    let current = f.ownership(&f.prepare(CURRENT, 1));
    let memo = HistoricalRevisionMemo::new(&f.engine);
    let (trace, _hook) = trace(&f);
    assert_physical_reads(&f, &current, &memo, &historical, 2);
    assert_physical_reads(&f, &current, &memo, &historical, 2);
    let note = f.note(&historical);
    let bytes = fs::read(&note).unwrap();
    let replacement = f.root().join("replacement.tmp");
    fs::write(&replacement, &bytes).unwrap();
    fs::rename(&replacement, &note).unwrap();
    assert_physical_reads(&f, &current, &memo, &historical, 2);
    let trace = trace.lock().unwrap();
    assert_eq!(trace.discoveries, vec![2, 2, 2]);
    assert_eq!(
        trace
            .lookups
            .iter()
            .map(|(_, hit)| *hit)
            .collect::<Vec<_>>(),
        vec![None, Some(false), Some(false)]
    );
}

#[test]
fn changed_reformatted_and_malformed_notes_are_reinterpreted_and_errors_are_not_cached() {
    let f = Fixture::new();
    let historical = f.prepare(HISTORY, 1);
    let current = f.ownership(&f.prepare(CURRENT, 1));
    let memo = HistoricalRevisionMemo::new(&f.engine);
    let (trace, _hook) = trace(&f);
    f.check(&current, &memo).unwrap();
    f.check(&current, &memo).unwrap();
    let note = f.note(&historical);
    let original = fs::read(&note).unwrap();
    // Frontmatter quote style changes complete bytes without changing manifest identity.
    let reformatted = String::from_utf8(original.clone())
        .unwrap()
        .replace("wiki_schema: \"1\"", "wiki_schema: '1'");
    assert_ne!(reformatted.as_bytes(), original);
    fs::write(&note, &reformatted).unwrap();
    f.check(&current, &memo).unwrap();
    f.check(&current, &memo).unwrap();
    for malformed in [
        b"unrecognized replacement".as_slice(),
        b"---\nwiki_id: somebody_else\n---\n",
    ] {
        fs::write(&note, malformed).unwrap();
        f.same_refusal(&current, &memo);
        assert_eq!(fs::read(&note).unwrap(), malformed);
    }
    // A repaired observation must succeed after failures; they cannot become negative entries.
    fs::write(&note, &original).unwrap();
    f.check(&current, &memo).unwrap();
    let hits: Vec<_> = trace
        .lock()
        .unwrap()
        .lookups
        .iter()
        .map(|(_, hit)| *hit)
        .collect();
    assert_eq!(&hits[..4], &[None, Some(false), None, Some(false)]);
    assert_eq!(hits.last(), Some(&None));

    // A valid manifest edit that introduces overlap must not retain the old non-overlap answer.
    let mut manifest = historical.manifest.clone();
    manifest.operations[0].target = relative(format!("{CURRENT}/0.bin"));
    let hash = Blake3Hash::digest(serde_json::to_vec(&manifest).unwrap());
    fs::write(
        &note,
        crate::changes::prepare::render_prepared_note(&manifest, &hash).unwrap(),
    )
    .unwrap();
    f.same_refusal(&current, &memo); // The original journal is bound to the old manifest hash.
}

#[test]
fn warmed_history_does_not_hide_new_history_or_missing_manifest_evidence() {
    let f = Fixture::new();
    let historical = f.prepare(HISTORY, 1);
    let current = f.ownership(&f.prepare(CURRENT, 1));
    let memo = HistoricalRevisionMemo::new(&f.engine);
    f.check(&current, &memo).unwrap();
    f.check(&current, &memo).unwrap();
    let note = f.note(&historical);
    let original = fs::read(&note).unwrap();
    fs::remove_file(&note).unwrap();
    assert_eq!(
        f.same_refusal(&current, &memo).code,
        ErrorCode::RecoveryRequired
    );
    fs::write(&note, &original).unwrap();
    let added = f.prepare("sources/source_new/revisions/revision_new", 1);
    fs::write(f.note(&added), b"new malformed history must be seen").unwrap();
    f.same_refusal(&current, &memo);
    assert_eq!(
        fs::read(f.note(&added)).unwrap(),
        b"new malformed history must be seen"
    );
    // Complete removal is rediscovered too; stale cached entries do not force loading vanished history.
    fs::remove_dir_all(f.note(&added).parent().unwrap()).unwrap();
    fs::remove_file(
        f.root().join(
            journal::journal_path(&added.prepared.change_id)
                .unwrap()
                .as_str(),
        ),
    )
    .unwrap();
    f.check(&current, &memo).unwrap();
}

#[test]
fn overlapping_hits_reread_terminal_journal_payload_and_lost_journal_proof() {
    for mutation in ["payload", "journal", "applying", "terminal", "ownership"] {
        let f = Fixture::new();
        let historical = f.prepare(CURRENT, 1);
        let current = f.ownership(&f.prepare(CURRENT, 1));
        let memo = HistoricalRevisionMemo::new(&f.engine);
        let (trace, _hook) = trace(&f);
        f.check(&current, &memo).unwrap(); // Prepared overlapping proposals are not owners yet.
        f.check(&current, &memo).unwrap();
        let journal_path = f.root().join(
            journal::journal_path(&historical.prepared.change_id)
                .unwrap()
                .as_str(),
        );
        let touched = match mutation {
            "payload" => {
                let p = f.root().join(
                    historical.manifest.operations[0]
                        .after_payload
                        .as_ref()
                        .unwrap()
                        .path
                        .as_str(),
                );
                fs::write(&p, b"unknown payload bytes").unwrap();
                p
            }
            "journal" => {
                fs::write(&journal_path, [b'x'; 80]).unwrap();
                journal_path.clone()
            }
            "applying" => {
                journal::append_event(
                    &f.engine.fs,
                    &f.writer,
                    &historical.manifest,
                    &historical.prepared.manifest_hash,
                    ChangeEvent::Applying,
                )
                .unwrap();
                journal_path.clone()
            }
            "terminal" => {
                let p = f.root().join(format!(
                    "changes/{}/outcome.json",
                    historical.prepared.change_id
                ));
                fs::write(&p, b"unknown terminal bytes").unwrap();
                p
            }
            "ownership" => {
                fs::remove_file(&journal_path).unwrap();
                let p = f.root().join(
                    receipt_path(&historical.prepared.change_id)
                        .unwrap()
                        .as_str(),
                );
                fs::write(&p, b"unknown ownership bytes").unwrap();
                p
            }
            _ => unreachable!(),
        };
        let bytes = fs::read(&touched).unwrap();
        f.same_refusal(&current, &memo);
        assert_eq!(
            fs::read(&touched).unwrap(),
            bytes,
            "{mutation} evidence must survive refusal"
        );
        assert_eq!(
            trace.lock().unwrap().lookups.last().unwrap().1,
            Some(true),
            "{mutation} must actually exercise a warmed overlap"
        );
        assert!(!f.root().join(format!("{CURRENT}/0.bin")).exists());
    }
}

#[test]
fn warmed_inventory_recomputes_overlap_for_current_roots_and_discovers_new_owners() {
    let f = Fixture::new();
    let historical = f.prepare(HISTORY, 1);
    let mut current = f.ownership(&f.prepare(CURRENT, 1));
    let memo = HistoricalRevisionMemo::new(&f.engine);
    let (trace, _hook) = trace(&f);
    f.check(&current, &memo).unwrap();
    f.check(&current, &memo).unwrap();
    journal::append_event(
        &f.engine.fs,
        &f.writer,
        &historical.manifest,
        &historical.prepared.manifest_hash,
        ChangeEvent::Applying,
    )
    .unwrap();
    // Nonoverlapping active history remains irrelevant, as in the original guard.
    f.check(&current, &memo).unwrap();
    current.trees = tree_inventory(&historical.manifest).unwrap();
    assert_eq!(
        f.same_refusal(&current, &memo).code,
        ErrorCode::ContentConflict
    );
    assert_eq!(trace.lock().unwrap().lookups.last().unwrap().1, Some(true));
    current.trees[0].root = relative(CURRENT);
    let added = f.prepare(CURRENT, 1);
    journal::append_event(
        &f.engine.fs,
        &f.writer,
        &added.manifest,
        &added.prepared.manifest_hash,
        ChangeEvent::Applying,
    )
    .unwrap();
    assert_eq!(
        f.same_refusal(&current, &memo).code,
        ErrorCode::ContentConflict
    );
    assert!(trace.lock().unwrap().discoveries.contains(&3));
}

#[test]
fn valid_aborted_terminal_is_rechecked_on_warmed_overlap() {
    let f = Fixture::new();
    let historical = f.prepare(CURRENT, 1);
    f.engine.abort(&f.writer, &historical.prepared).unwrap();
    let current = f.ownership(&f.prepare(CURRENT, 1));
    let memo = HistoricalRevisionMemo::new(&f.engine);
    f.check(&current, &memo).unwrap();
    f.check(&current, &memo).unwrap();
    let terminal = f.root().join(format!(
        "changes/{}/outcome.json",
        historical.prepared.change_id
    ));
    fs::write(&terminal, b"externally changed aborted receipt").unwrap();
    f.same_refusal(&current, &memo);
    assert_eq!(
        fs::read(terminal).unwrap(),
        b"externally changed aborted receipt"
    );
}

#[test]
fn memo_bindings_reject_other_vaults_and_change_ids() {
    let f = Fixture::new();
    let historical = f.prepare(HISTORY, 1);
    let current = f.ownership(&f.prepare(CURRENT, 1));
    let memo = HistoricalRevisionMemo::new(&f.engine);
    f.check(&current, &memo).unwrap();
    let bytes = fs::read(f.note(&historical)).unwrap();
    assert_eq!(
        memo.overlap(&f.engine, &historical.prepared.change_id, &bytes, |_| false),
        Some(false)
    );
    assert_eq!(
        memo.overlap(
            &f.engine,
            &RecordId::new("change_other").unwrap(),
            &bytes,
            |_| false
        ),
        None
    );
    let other = Fixture::new(); // Same vault ID, different root must still miss.
    assert_eq!(
        memo.overlap(
            &other.engine,
            &historical.prepared.change_id,
            &bytes,
            |_| false
        ),
        None
    );
    let other_identity = ChangeEngine {
        fs: f.engine.fs.clone(),
        vault_id: RecordId::new("vault_other").unwrap(),
    };
    assert_eq!(
        memo.overlap(
            &other_identity,
            &historical.prepared.change_id,
            &bytes,
            |_| false
        ),
        None
    );
    // A fresh invocation also starts cold, even for the same engine and bytes.
    assert_eq!(
        HistoricalRevisionMemo::new(&f.engine).overlap(
            &f.engine,
            &historical.prepared.change_id,
            &bytes,
            |_| false
        ),
        None
    );
}

#[test]
fn entry_and_retained_byte_caps_fall_back_to_original_validation() {
    let f = Fixture::new();
    let historical = f.prepare(HISTORY, 1);
    let current = f.ownership(&f.prepare(CURRENT, 1));
    for byte_cap in [false, true] {
        let memo = HistoricalRevisionMemo::new(&f.engine);
        // Validated synthetic histories exercise the retained representation limit without
        // manufacturing hundreds of durable proposals or timing their filesystem work.
        for n in 0..if byte_cap { 20 } else { 257 } {
            let mut manifest = historical.manifest.clone();
            manifest.change_id = RecordId::new(format!("change_capacity_{n:03}")).unwrap();
            for op in &mut manifest.operations {
                if let Some(payload) = &mut op.after_payload {
                    payload.path = relative(payload.path.as_str().replace(
                        historical.prepared.change_id.as_str(),
                        manifest.change_id.as_str(),
                    ));
                }
            }
            let hash = Blake3Hash::digest(serde_json::to_vec(&manifest).unwrap());
            let mut bytes =
                crate::changes::prepare::render_prepared_note(&manifest, &hash).unwrap();
            if byte_cap {
                bytes.resize(1024 * 1024, b' ');
            }
            let (decoded, _) = f
                .engine
                .decode_manifest_structure_observed(&manifest.change_id, &bytes)
                .unwrap();
            let roots = tree_inventory(&decoded)
                .unwrap()
                .into_iter()
                .map(|tree| tree.root)
                .collect();
            memo.remember(&f.engine, &manifest.change_id, bytes, roots);
        }
        let (entries, retained) = memo.retained_usage();
        assert!(retained <= 16 * 1024 * 1024);
        if byte_cap {
            assert!(entries > 0 && entries < 20);
        } else {
            assert_eq!(entries, 256);
        }
        // A huge-capacity buffer with a short valid witness must be charged by allocation.
        let mut oversized = Vec::with_capacity(16 * 1024 * 1024);
        oversized.extend(fs::read(f.note(&historical)).unwrap());
        memo.remember(
            &f.engine,
            &historical.prepared.change_id,
            oversized,
            tree_inventory(&historical.manifest)
                .unwrap()
                .into_iter()
                .map(|tree| tree.root)
                .collect(),
        );
        assert_eq!(memo.retained_usage(), (entries, retained));
        let mut candidate = fs::read(f.note(&historical)).unwrap();
        if byte_cap {
            candidate.resize(2 * 1024 * 1024, b' ');
        }
        fs::write(f.note(&historical), &candidate).unwrap();
        f.check(&current, &memo).unwrap();
        assert_eq!(
            memo.retained_usage(),
            (entries, retained),
            "a full memo falls back without retaining this history"
        );
        fs::write(f.note(&historical), b"bad manifest after cap exhaustion").unwrap();
        f.same_refusal(&current, &memo);
        fs::write(
            f.note(&historical),
            crate::changes::prepare::render_prepared_note(
                &historical.manifest,
                &historical.prepared.manifest_hash,
            )
            .unwrap(),
        )
        .unwrap();
    }
}

struct Validator;
impl GraphValidator for Validator {
    fn validate(&self, _: &VaultFs, _: &ValidationInput) -> Result<ValidatedGraph> {
        Ok(ValidatedGraph {
            parser_fingerprint: Blake3Hash::digest(b"memo fixture parser"),
            control_manifest: Blake3Hash::digest(b"memo fixture graph"),
            dependencies: vec![],
        })
    }
}
#[derive(Default)]
struct Publisher(AtomicUsize);
impl PublicationBackend for Publisher {
    fn check_available(&self) -> Result<()> {
        Ok(())
    }
    fn publish(
        &self,
        _: &VaultFs,
        permit: &PublicationPermit<'_>,
        _: &ValidationInput,
    ) -> Result<ReadSnapshot> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ReadSnapshot::canonical(
            1,
            permit.graph().parser_fingerprint.clone(),
            permit.graph().control_manifest.clone(),
        ))
    }
}
#[test]
fn apply_detects_history_edit_at_original_next_guard_and_preserves_unknown_bytes() {
    for after_first_write in [false, true] {
        let f = Fixture::new();
        let historical = f.prepare(HISTORY, 1);
        let current = f.prepare(CURRENT, 2);
        let note = f.note(&historical);
        let changed = note.clone();
        let state = Arc::new(Mutex::new((0usize, 0usize, false)));
        let counted = state.clone();
        let _hook = hooks::install(f.engine.fs.root(), move |event| {
            let mut state = counted.lock().unwrap();
            if matches!(event, Event::HistoricalLookup(_, Some(false))) {
                state.0 += 1;
            }
            if matches!(event, Event::BeforeMutation(_)) {
                state.1 += 1;
            }
            let inject = if after_first_write {
                matches!(event, Event::BeforeMutation(0))
            } else {
                matches!(event, Event::LoopBegin)
            };
            if inject && !state.2 {
                fs::write(&changed, b"external history edit must survive").unwrap();
                state.2 = true;
            }
            Ok(())
        });
        let publisher = Publisher::default();
        let error = f
            .engine
            .apply(&f.writer, &current.prepared, &Validator, &publisher)
            .unwrap_err();
        assert!(matches!(error.code, ErrorCode::ContentConflict));
        let state = state.lock().unwrap();
        assert!(state.2);
        if after_first_write {
            assert!(state.0 > 0, "edit must follow actual warmed hits");
        }
        assert_eq!(state.1, usize::from(after_first_write));
        assert_eq!(publisher.0.load(Ordering::SeqCst), 0);
        assert_eq!(
            fs::read(note).unwrap(),
            b"external history edit must survive"
        );
        assert!(!f.root().join(format!("{CURRENT}/1.bin")).exists());
        let first = f.root().join(format!("{CURRENT}/0.bin"));
        if after_first_write {
            assert_eq!(fs::read(first).unwrap(), b"member 0");
        } else {
            assert!(!first.exists());
        }
        let journal = journal::load_journal(
            &f.engine.fs,
            &current.manifest,
            &current.prepared.manifest_hash,
        )
        .unwrap();
        assert_eq!(journal.status, Some(ChangeStatus::Conflict));
        assert!(
            !journal
                .frames
                .iter()
                .any(|frame| matches!(frame.event, ChangeEvent::Done { .. })),
            "the immediate post-write guard precedes Done journaling"
        );
    }
}

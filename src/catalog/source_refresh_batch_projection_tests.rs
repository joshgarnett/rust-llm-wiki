// Included beside the scalar tests to share their disposable vault and full
// reconstruction oracle without duplicating either fixture or scalar tests.
fn batch_other_source(fixture: &Fixture) -> RecordId {
    let parsed = parse_note(&fs::read(fixture.fs.root().path().join("evidence_other.md")).unwrap());
    record_id(parsed.canonical.as_ref().unwrap(), "wiki_source_id").unwrap()
}

fn batch_plans(
    fixture: &Fixture,
    requests: &[(&RecordId, &[u8], Option<&str>)],
) -> (QuerySnapshot, Vec<IndexedSourceRefreshPlan>) {
    let reader = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let store = SourceStore::new(fixture.fs.clone());
    let plans = requests
        .iter()
        .map(|(source, bytes, title)| {
            store
                .plan_refresh_indexed(
                    &reader,
                    source,
                    Fixture::request(bytes),
                    *title,
                    &SourceRefreshLimits::default(),
                )
                .unwrap()
        })
        .collect();
    (reader, plans)
}

fn batch_apply(
    fixture: &Fixture,
    requests: &[(&RecordId, &[u8], Option<&str>)],
) -> (CatalogDelta, crate::changes::PreparedChange) {
    let (reader, plans) = batch_plans(fixture, requests);
    let projected = project_refresh_batch(
        &fixture.fs,
        &reader,
        plans,
        &RefreshProjectionLimits::default(),
    )
    .unwrap()
    .unwrap();
    let parts = projected.into_parts();
    let delta = parts.delta.clone();
    let mut session = IndexedRefreshSession::prepare_write(
        &fixture.catalog,
        &fixture.writer,
        super::super::write_projection::ProjectedWrite::from_parts(parts),
    )
    .unwrap();
    let engine = ChangeEngine::new(fixture.fs.clone()).unwrap();
    let report = engine
        .apply_indexed_refresh(&fixture.writer, &mut session)
        .unwrap();
    assert_eq!(report.status, ChangeStatus::Committed);
    (delta, report.change)
}

fn batch_fanout_seed(fixture: &Fixture) {
    let other = batch_other_source(fixture);
    let parsed = parse_note(&fs::read(fixture.fs.root().path().join("evidence_other.md")).unwrap());
    let other_revision =
        record_id(parsed.canonical.as_ref().unwrap(), "wiki_source_revision").unwrap();
    let fingerprint = Blake3Hash::digest(b"fixture packet");
    let packet = RecordId::packet(&fingerprint);
    fixture.write(
        "runs/run_batch/run.md",
        &note(
            "run",
            "run_batch",
            json!({
                "wiki_status":"completed", "wiki_created_at":"2026-09-28T00:00:00Z"
            }),
            b"Run",
        ),
    );
    let task = Blake3Hash::digest(b"batch generation task");
    let output = crate::graph::generation_cache::GenerationOutput {
        version: 1,
        task_key: task.clone(),
        packet_id: packet.clone(),
        packet_fingerprint: fingerprint.clone(),
        attempt: crate::jobs::AttemptRef {
            run_id: id("run_batch"),
            task_key: task,
            attempt_id: id("attempt_batch"),
            number: 1,
            request_hash: Blake3Hash::digest(b"request"),
        },
        response: "Response".into(),
        response_hash: Blake3Hash::digest(b"Response"),
    };
    fixture.write(
        "runs/run_batch/outputs/event_batch.md",
        &note(
            "run_event",
            "event_batch",
            json!({
                "wiki_run_id":"run_batch", "wiki_sequence":1, "wiki_event_type":"generation_output",
                "wiki_occurred_at":"2026-09-28T00:00:00Z"
            }),
            &crate::graph::packet::render_fence(
                &output,
                "lwiki-api-extraction-output-v1",
                crate::graph::MAX_ARTIFACT_BYTES,
            )
            .unwrap(),
        ),
    );
    fixture.write("extraction-batch.md", &note("extraction", "extraction_batch", json!({
            "wiki_status":"completed", "wiki_packet_id":packet, "wiki_input_hash":fingerprint,
            "wiki_extractor_fingerprint":Blake3Hash::digest(b"extractor"), "wiki_executor":"agent",
            "wiki_source_ids":[fixture.source, other],
            "wiki_source_revision_ids":[fixture.first, other_revision],
            "wiki_completed_at":"2026-09-28T00:00:00Z"
        }), b"Extraction across both selected Sources"));
}

fn batch_fanout_fixture() -> Fixture {
    Fixture::with_setup(true, batch_fanout_seed)
}

#[test]
fn refresh_batch_shared_support_opposition_generation_matches_final_reconstruction() {
    let fixture = batch_fanout_fixture();
    let other = batch_other_source(&fixture);
    let (delta, _) = batch_apply(
        &fixture,
        &[
            (&other, b"other changed capture", None),
            (&fixture.source, b"first changed capture", None),
        ],
    );
    let record_ids: BTreeSet<_> = delta.records.iter().map(|row| row.record.id()).collect();
    assert_eq!(
        record_ids.len(),
        delta.records.len(),
        "shared dependents emitted once"
    );
    assert_eq!(delta.revisions.len(), 2);
    fixture.oracle();
    let reader = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    for assertion in ["assertion_keep", "assertion_opposite"] {
        let row = reader.record(&id(assertion)).unwrap().unwrap();
        assert_eq!(row.eligibility, Eligibility::Stale);
        assert!(!row.disputed);
    }
    assert_eq!(
        reader
            .record(&id("event_batch"))
            .unwrap()
            .unwrap()
            .eligibility,
        Eligibility::Historical
    );
    assert_eq!(
        reader
            .record(&id("extraction_batch"))
            .unwrap()
            .unwrap()
            .eligibility,
        Eligibility::Historical
    );
    drop(reader);
    let (historical, _) = batch_apply(
        &fixture,
        &[
            (&fixture.source, b"first capture quote", None),
            (&other, b"other capture quote", None),
        ],
    );
    assert!(historical.revisions.is_empty());
    fixture.oracle();
    let reader = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    assert!(
        reader
            .record(&id("assertion_keep"))
            .unwrap()
            .unwrap()
            .disputed
    );
    assert_eq!(
        reader
            .record(&id("event_batch"))
            .unwrap()
            .unwrap()
            .eligibility,
        Eligibility::Unsupported
    );
    assert_eq!(
        reader
            .record(&id("extraction_batch"))
            .unwrap()
            .unwrap()
            .eligibility,
        Eligibility::Unsupported
    );
}

#[test]
fn refresh_batch_mixed_noop_retains_rows_guards_and_historical_terminal_replay() {
    use crate::changes::indexed_refresh::IndexedWriteOperation;
    let fixture = Fixture::new(true);
    let other = batch_other_source(&fixture);
    let (reader, plans) = batch_plans(
        &fixture,
        &[
            (&other, b"changed other", None),
            (&fixture.source, b"first capture quote", None),
        ],
    );
    let projected = project_refresh_batch(
        &fixture.fs,
        &reader,
        plans,
        &RefreshProjectionLimits::default(),
    )
    .unwrap()
    .unwrap();
    let parts = projected.into_parts();
    let IndexedWriteOperation::SourceRefreshBatch { refreshes, .. } = &parts.operation else {
        panic!("batch descriptor");
    };
    assert_eq!(refreshes.len(), 2);
    assert!(
        refreshes
            .windows(2)
            .all(|pair| pair[0].source_id < pair[1].source_id)
    );
    let noop = refreshes
        .iter()
        .find(|target| target.source_id == fixture.source)
        .unwrap();
    assert!(noop.no_op && noop.reused);
    let retained = noop.unchanged_source.as_ref().unwrap();
    assert_eq!(
        parse_note(retained.as_bytes()).canonical.as_ref(),
        Some(&reader.record(&fixture.source).unwrap().unwrap().record)
    );
    assert_eq!(noop.previous_revision_id, noop.revision_id);
    assert_eq!(
        parts
            .delta
            .records
            .iter()
            .filter(|row| row.record.kind() == RecordKind::Source)
            .count(),
        2
    );
    let unchanged = parts
        .delta
        .records
        .iter()
        .find(|row| row.record.id() == &fixture.source)
        .unwrap();
    assert_eq!(
        Some(unchanged),
        reader.record(&fixture.source).unwrap().as_ref()
    );
    for record in [&fixture.source, &fixture.first] {
        let row = reader.record(record).unwrap().unwrap();
        assert!(parts.before.iter().any(
            |dep| dep.path == row.path && dep.expected == ExpectedState::Hash(row.hash.clone())
        ));
    }
    let fresh_index = refreshes.iter().position(|target| !target.reused).unwrap();
    assert_eq!(
        parts.draft.allocated_ids,
        BTreeMap::from([(
            format!("revision_{fresh_index}"),
            refreshes[fresh_index].revision_id.clone(),
        )])
    );
    let mut session = IndexedRefreshSession::prepare_write(
        &fixture.catalog,
        &fixture.writer,
        super::super::write_projection::ProjectedWrite::from_parts(parts),
    )
    .unwrap();
    let engine = ChangeEngine::new(fixture.fs.clone()).unwrap();
    let report = engine
        .apply_indexed_refresh(&fixture.writer, &mut session)
        .unwrap();
    drop(session);
    drop(reader);
    fixture.oracle();
    fixture
        .apply(
            Fixture::request(b"later independent first Source refresh"),
            None,
        )
        .unwrap();
    assert_eq!(
        engine
            .indexed_refresh_terminal_outcome(&report.change)
            .unwrap(),
        Some(report)
    );
}

#[test]
fn refresh_batch_mixed_noop_source_drift_blocks_every_mutation() {
    let fixture = Fixture::new(true);
    let other = batch_other_source(&fixture);
    let (reader, plans) = batch_plans(
        &fixture,
        &[
            (&fixture.source, b"first capture quote", None),
            (&other, b"changed other", None),
        ],
    );
    let projected = project_refresh_batch(
        &fixture.fs,
        &reader,
        plans,
        &RefreshProjectionLimits::default(),
    )
    .unwrap()
    .unwrap();
    let mut session =
        IndexedRefreshSession::prepare_write(&fixture.catalog, &fixture.writer, projected).unwrap();
    let other_path = reader.record(&other).unwrap().unwrap().path;
    let other_before = fs::read(fixture.fs.root().path().join(other_path.as_str())).unwrap();
    let source_path = reader.record(&fixture.source).unwrap().unwrap().path;
    let mut drift = fs::read(fixture.fs.root().path().join(source_path.as_str())).unwrap();
    drift.extend(b"external edit");
    fixture.write(source_path.as_str(), &drift);
    let engine = ChangeEngine::new(fixture.fs.clone()).unwrap();
    assert!(
        engine
            .apply_indexed_refresh(&fixture.writer, &mut session)
            .is_err()
    );
    assert_eq!(
        fs::read(fixture.fs.root().path().join(other_path.as_str())).unwrap(),
        other_before
    );
}

#[test]
fn refresh_batch_all_noop_rechecks_every_captured_guard() {
    let fixture = Fixture::new(true);
    let other = batch_other_source(&fixture);
    let requests = [
        (&fixture.source, b"first capture quote".as_slice(), None),
        (&other, b"other capture quote".as_slice(), None),
    ];
    let (reader, plans) = batch_plans(&fixture, &requests);
    assert!(
        project_refresh_batch(
            &fixture.fs,
            &reader,
            plans,
            &RefreshProjectionLimits::default()
        )
        .unwrap()
        .is_none()
    );
    let (reader, plans) = batch_plans(&fixture, &requests);
    fixture.write(
        "WIKI.md",
        &note("vault", "vault_projector", json!({}), b"Header drift"),
    );
    assert!(
        project_refresh_batch(
            &fixture.fs,
            &reader,
            plans,
            &RefreshProjectionLimits::default()
        )
        .is_err()
    );
}

#[test]
fn refresh_batch_title_only_and_historical_reuse_preserve_immutable_inventory() {
    let fixture = Fixture::new(true);
    let other = batch_other_source(&fixture);
    let (delta, _) = batch_apply(
        &fixture,
        &[
            (&fixture.source, b"first capture quote", Some("Batch title")),
            (&other, b"other capture quote", Some("Other title")),
        ],
    );
    assert!(delta.revisions.is_empty());
    fixture.oracle();
    let (reader, plans) = batch_plans(
        &fixture,
        &[
            (&fixture.source, b"first capture quote", Some("Batch title")),
            (&other, b"other capture quote", Some("Other title")),
        ],
    );
    assert!(
        project_refresh_batch(
            &fixture.fs,
            &reader,
            plans,
            &RefreshProjectionLimits::default()
        )
        .unwrap()
        .is_none()
    );
}

#[test]
fn refresh_batch_identical_overlaps_are_metered_and_conflicting_overlap_is_refused() {
    let fixture = Fixture::new(true);
    let other = batch_other_source(&fixture);
    let requests = [
        (&fixture.source, b"new first".as_slice(), None),
        (&other, b"new other".as_slice(), None),
    ];
    let (reader, plans) = batch_plans(&fixture, &requests);
    let wiki: Vec<_> = plans
        .iter()
        .map(|plan| {
            plan.captured
                .iter()
                .find(|doc| doc.path.as_str() == "WIKI.md")
                .unwrap()
        })
        .collect();
    assert_eq!(wiki[0].bytes, wiki[1].bytes);
    assert!(
        project_refresh_batch(
            &fixture.fs,
            &reader,
            plans,
            &RefreshProjectionLimits::default()
        )
        .unwrap()
        .is_some()
    );
    let (reader, mut plans) = batch_plans(&fixture, &requests);
    let wiki = plans[1]
        .captured
        .iter_mut()
        .find(|doc| doc.path.as_str() == "WIKI.md")
        .unwrap();
    wiki.bytes.extend(b"different overlap");
    wiki.hash = Blake3Hash::digest(&wiki.bytes);
    assert!(
        project_refresh_batch(
            &fixture.fs,
            &reader,
            plans,
            &RefreshProjectionLimits::default()
        )
        .is_err()
    );
    let (reader, plans) = batch_plans(&fixture, &requests);
    let bytes: usize = plans
        .iter()
        .flat_map(|plan| &plan.captured)
        .map(|doc| doc.bytes.len())
        .sum();
    let limits = RefreshProjectionLimits {
        max_canonical_bytes: bytes,
        ..RefreshProjectionLimits::default()
    };
    let error = project_refresh_batch(&fixture.fs, &reader, plans, &limits)
        .err()
        .unwrap();
    assert_eq!(error.code, ErrorCode::BudgetExceeded);
}

#[test]
fn refresh_batch_duplicate_source_and_cross_source_revision_collision_are_refused() {
    let fixture = Fixture::new(true);
    let other = batch_other_source(&fixture);
    let (reader, plans) = batch_plans(
        &fixture,
        &[
            (&fixture.source, b"new first", None),
            (&fixture.source, b"new first", None),
        ],
    );
    assert!(
        project_refresh_batch(
            &fixture.fs,
            &reader,
            plans,
            &RefreshProjectionLimits::default()
        )
        .is_err()
    );
    let (reader, mut plans) = batch_plans(
        &fixture,
        &[
            (&fixture.source, b"new first", None),
            (&other, b"new other", None),
        ],
    );
    plans[1].plan.revision_id = plans[0].plan.revision_id.clone();
    assert!(
        project_refresh_batch(
            &fixture.fs,
            &reader,
            plans,
            &RefreshProjectionLimits::default()
        )
        .is_err()
    );
}

#[test]
fn refresh_batch_fresh_foreign_owner_and_extra_write_are_refused_before_mutation() {
    let fixture = Fixture::new(true);
    let other = batch_other_source(&fixture);
    let requests = [
        (&fixture.source, b"new first".as_slice(), None),
        (&other, b"new other".as_slice(), None),
    ];
    let source_path = format!("sources/{}/source.md", fixture.source);
    let before = fs::read(fixture.fs.root().path().join(&source_path)).unwrap();
    let (reader, mut plans) = batch_plans(&fixture, &requests);
    let op = plans[0]
        .plan
        .draft
        .as_mut()
        .unwrap()
        .operations
        .iter_mut()
        .find(|op| op.target.as_str().ends_with("/revision.md"))
        .unwrap();
    let parsed = parse_note(op.proposed.as_ref().unwrap());
    let mut fields = parsed.canonical.as_ref().unwrap().fields().clone();
    fields.insert("wiki_source_id".into(), json!(other));
    op.proposed = Some(
        crate::sources::revision::record_bytes(
            CanonicalRecord::new(fields).unwrap(),
            parsed.body(),
        )
        .unwrap(),
    );
    assert!(
        project_refresh_batch(
            &fixture.fs,
            &reader,
            plans,
            &RefreshProjectionLimits::default()
        )
        .is_err()
    );
    let (reader, mut plans) = batch_plans(&fixture, &requests);
    let mut extra = plans[0].plan.draft.as_ref().unwrap().operations[0].clone();
    extra.target = path("unrelated-new.md");
    extra.expected = ExpectedState::Absent;
    extra.apply_after.clear();
    plans[0].plan.draft.as_mut().unwrap().operations.push(extra);
    assert!(
        project_refresh_batch(
            &fixture.fs,
            &reader,
            plans,
            &RefreshProjectionLimits::default()
        )
        .is_err()
    );
    assert_eq!(
        fs::read(fixture.fs.root().path().join(source_path)).unwrap(),
        before
    );
    assert!(!fixture.fs.root().path().join("unrelated-new.md").exists());
}

#[test]
fn refresh_batch_single_member_keeps_scalar_delta_and_scalar_allocation_contract() {
    let fixture = Fixture::new(true);
    let (reader, plan) = fixture.plan(Fixture::request(b"same frozen candidate"), None);
    let twin = IndexedSourceRefreshPlan {
        plan: plan.plan.clone(),
        base_snapshot: plan.base_snapshot.clone(),
        previous_revision: plan.previous_revision.clone(),
        captured: plan.captured.clone(),
    };
    let scalar = project_refresh(
        &fixture.fs,
        &reader,
        plan,
        &RefreshProjectionLimits::default(),
    )
    .unwrap()
    .unwrap()
    .into_parts();
    let batch = project_refresh_batch(
        &fixture.fs,
        &reader,
        vec![twin],
        &RefreshProjectionLimits::default(),
    )
    .unwrap()
    .unwrap()
    .into_parts();
    assert_eq!(scalar.delta, batch.delta);
    assert_eq!(scalar.before, batch.before);
    assert_eq!(scalar.after, batch.after);
    assert_eq!(scalar.draft.operations.len(), batch.draft.operations.len());
    assert_eq!(
        scalar
            .draft
            .allocated_ids
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["revision"]
    );
    assert_eq!(
        batch
            .draft
            .allocated_ids
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["revision_0"]
    );
    assert_eq!(
        scalar.draft.allocated_ids["revision"],
        batch.draft.allocated_ids["revision_0"]
    );
}

// These adapters mirror the repository's DurableIo journal/rename faults and
// native self-spawn marker protocol. A returned error is a separate test from
// the enabled Unix wrapper that actually kills a child at the reached boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BatchRecoveryCut {
    RetainedBaseline,
    ActiveIntent,
    FirstAsset,
    FirstSource,
    FilesApplied,
    SqlBeforeCommit,
    SqlAfterCommit,
    TerminalReceipt,
}
impl BatchRecoveryCut {
    fn name(self) -> &'static str {
        match self {
            Self::RetainedBaseline => "retained_baseline",
            Self::ActiveIntent => "active_intent",
            Self::FirstAsset => "first_asset",
            Self::FirstSource => "between_source_pointers",
            Self::FilesApplied => "files_applied",
            Self::SqlBeforeCommit => "sql_before_commit",
            Self::SqlAfterCommit => "sql_after_commit",
            Self::TerminalReceipt => "terminal_receipt_before_authority_cleanup",
        }
    }
    fn parse(name: &str) -> Self {
        [
            Self::RetainedBaseline,
            Self::ActiveIntent,
            Self::FirstAsset,
            Self::FirstSource,
            Self::FilesApplied,
            Self::SqlBeforeCommit,
            Self::SqlAfterCommit,
            Self::TerminalReceipt,
        ]
        .into_iter()
        .find(|cut| cut.name() == name)
        .unwrap()
    }
}
struct BatchCutControl {
    cut: BatchRecoveryCut,
    fired: std::sync::atomic::AtomicBool,
    marker: Option<std::path::PathBuf>,
    proof_file: std::path::PathBuf,
}
impl BatchCutControl {
    fn reached(&self) -> std::io::Result<()> {
        use std::sync::atomic::Ordering;
        if self.fired.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        if let Some(marker) = &self.marker {
            fs::write(marker, self.cut.name())?;
            loop {
                std::thread::park_timeout(Duration::from_millis(10));
            }
        }
        Err(std::io::Error::other(format!(
            "injected returned error after {}",
            self.cut.name()
        )))
    }
}
struct BatchCutIo {
    control: std::sync::Arc<BatchCutControl>,
    journal_armed: std::sync::atomic::AtomicBool,
    terminal_parent: std::sync::Mutex<Option<std::path::PathBuf>>,
}
impl crate::vault::DurableIo for BatchCutIo {
    fn create_stage(&self, p: &std::path::Path) -> std::io::Result<fs::File> {
        crate::vault::DurableIo::create_stage(&crate::vault::NativeIo, p)
    }
    fn create_private_stage(&self, p: &std::path::Path) -> std::io::Result<fs::File> {
        crate::vault::DurableIo::create_private_stage(&crate::vault::NativeIo, p)
    }
    fn create_private_directory(&self, p: &std::path::Path) -> std::io::Result<()> {
        crate::vault::DurableIo::create_private_directory(&crate::vault::NativeIo, p)
    }
    fn open_append(&self, p: &std::path::Path) -> std::io::Result<fs::File> {
        crate::vault::DurableIo::open_append(&crate::vault::NativeIo, p)
    }
    fn truncate_file(&self, f: &fs::File, n: u64) -> std::io::Result<()> {
        crate::vault::DurableIo::truncate_file(&crate::vault::NativeIo, f, n)
    }
    fn write_stage(&self, f: &mut fs::File, b: &[u8]) -> std::io::Result<()> {
        crate::vault::DurableIo::write_stage(&crate::vault::NativeIo, f, b)?;
        let event = b"\"event\":\"files_applied\"";
        if self.control.cut == BatchRecoveryCut::FilesApplied
            && b.starts_with(b"LWJNL001")
            && b.windows(event.len()).any(|window| window == event)
        {
            self.journal_armed
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        Ok(())
    }
    fn sync_file(&self, f: &fs::File) -> std::io::Result<()> {
        crate::vault::DurableIo::sync_file(&crate::vault::NativeIo, f)?;
        if self
            .journal_armed
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            self.control.reached()?;
        }
        Ok(())
    }
    fn replace(&self, staged: &std::path::Path, target: &std::path::Path) -> std::io::Result<()> {
        crate::vault::DurableIo::replace(&crate::vault::NativeIo, staged, target)?;
        if self.control.cut == BatchRecoveryCut::TerminalReceipt
            && target
                .file_name()
                .is_some_and(|name| name == "outcome.json")
        {
            *self.terminal_parent.lock().unwrap() = Some(target.parent().unwrap().to_path_buf());
        }
        let selected =
            match self.control.cut {
                BatchRecoveryCut::RetainedBaseline => target
                    .file_name()
                    .is_some_and(|name| name == "validation.json"),
                BatchRecoveryCut::ActiveIntent => {
                    target
                        .file_name()
                        .is_some_and(|name| name == "operations.json")
                        && serde_json::from_slice::<serde_json::Value>(&fs::read(target)?).unwrap()
                            ["active"]
                            .is_object()
                }
                BatchRecoveryCut::FirstAsset => target
                    .file_name()
                    .is_some_and(|name| name == "original.bin"),
                BatchRecoveryCut::FirstSource => {
                    target.file_name().is_some_and(|name| name == "source.md")
                }
                _ => false,
            };
        if selected {
            if self.control.cut == BatchRecoveryCut::RetainedBaseline {
                let receipt: serde_json::Value =
                    serde_json::from_slice(&fs::read(target)?).unwrap();
                fs::write(
                    &self.control.proof_file,
                    serde_json::to_vec(&receipt["proof"]).unwrap(),
                )?;
            }
            self.control.reached()?;
        }
        Ok(())
    }
    fn remove(&self, p: &std::path::Path) -> std::io::Result<()> {
        crate::vault::DurableIo::remove(&crate::vault::NativeIo, p)
    }
    fn create_directory(&self, p: &std::path::Path) -> std::io::Result<()> {
        crate::vault::DurableIo::create_directory(&crate::vault::NativeIo, p)
    }
    fn sync_directory(&self, p: &std::path::Path) -> std::io::Result<crate::vault::DirectorySync> {
        let synced = crate::vault::DurableIo::sync_directory(&crate::vault::NativeIo, p)?;
        let terminal = {
            let mut parent = self.terminal_parent.lock().unwrap();
            if parent.as_deref() == Some(p) {
                parent.take();
                true
            } else {
                false
            }
        };
        if terminal {
            self.control.reached()?;
        }
        Ok(synced)
    }
}
struct BatchSqlCut(std::sync::Arc<BatchCutControl>);
impl crate::catalog::PublicationFault for BatchSqlCut {
    fn check(&self, point: crate::catalog::PublicationCheckpoint) -> Result<()> {
        use crate::catalog::PublicationCheckpoint;
        let selected = matches!(
            (self.0.cut, point),
            (
                BatchRecoveryCut::SqlBeforeCommit,
                PublicationCheckpoint::AfterPointer
            ) | (
                BatchRecoveryCut::SqlAfterCommit,
                PublicationCheckpoint::AfterCommit
            )
        );
        if selected {
            self.0
                .reached()
                .map_err(|error| WikiError::new(ErrorCode::RecoveryRequired, error.to_string()))?;
        }
        Ok(())
    }
}
fn batch_fault_catalog(fixture: &Fixture, control: &std::sync::Arc<BatchCutControl>) -> Catalog {
    let fault_fs = VaultFs::with_io(
        fixture.fs.root().clone(),
        std::sync::Arc::new(BatchCutIo {
            control: control.clone(),
            journal_armed: std::sync::atomic::AtomicBool::new(false),
            terminal_parent: std::sync::Mutex::new(None),
        }),
    );
    Catalog::with_options(
        fault_fs,
        id("vault_projector"),
        crate::catalog::CatalogOptions {
            busy_timeout_ms: 1000,
            fault: Some(std::sync::Arc::new(BatchSqlCut(control.clone()))),
        },
    )
}
fn batch_control(
    fixture: &Fixture,
    cut: BatchRecoveryCut,
    native: bool,
) -> std::sync::Arc<BatchCutControl> {
    std::sync::Arc::new(BatchCutControl {
        cut,
        fired: std::sync::atomic::AtomicBool::new(false),
        marker: native.then(|| fixture.fs.root().path().join(".wiki/batch-crash-ready")),
        proof_file: fixture
            .fs
            .root()
            .path()
            .join(".wiki/batch-crash-proof.json"),
    })
}
fn batch_project_two(
    fixture: &Fixture,
) -> (
    QuerySnapshot,
    super::super::write_projection::ProjectedWrite,
) {
    let other = batch_other_source(fixture);
    let (reader, plans) = batch_plans(
        fixture,
        &[
            (&fixture.source, b"new batch first", None),
            (&other, b"new batch other", None),
        ],
    );
    let projected = project_refresh_batch(
        &fixture.fs,
        &reader,
        plans,
        &RefreshProjectionLimits::default(),
    )
    .unwrap()
    .unwrap();
    (reader, projected)
}

fn batch_old_immutable_bytes(
    fixture: &Fixture,
    reader: &QuerySnapshot,
) -> BTreeMap<VaultRelativePath, Vec<u8>> {
    let other = batch_other_source(fixture);
    let mut files = BTreeMap::new();
    for source in [&fixture.source, &other] {
        let source_row = reader.record(source).unwrap().unwrap();
        for revision in scan::list(&source_row.record, "wiki_revisions") {
            let row = reader.record(&id(&revision)).unwrap().unwrap();
            for path in std::iter::once(row.path.clone()).chain(
                ["wiki_original_path", "wiki_content_path"]
                    .into_iter()
                    .filter_map(|field| row.record.string(field))
                    .map(|name| asset_path(&row, name).unwrap()),
            ) {
                files.insert(
                    path.clone(),
                    fs::read(fixture.fs.root().path().join(path.as_str())).unwrap(),
                );
            }
        }
    }
    files
}
fn batch_assert_preserved(
    fixture: &Fixture,
    old: &BTreeMap<VaultRelativePath, Vec<u8>>,
    unknown: &[u8],
) {
    for (path, bytes) in old {
        assert_eq!(
            fs::read(fixture.fs.root().path().join(path.as_str())).unwrap(),
            *bytes,
            "old immutable {path}"
        );
    }
    assert_eq!(
        fs::read(fixture.fs.root().path().join("unknown.bin")).unwrap(),
        unknown
    );
}
fn batch_recovery_targets(
    proof: &crate::changes::indexed_refresh::IndexedRefreshProof,
) -> &[crate::changes::indexed_refresh::IndexedRefreshTarget] {
    let crate::changes::indexed_refresh::IndexedWriteOperation::SourceRefreshBatch {
        refreshes,
        ..
    } = proof.operation.as_ref().unwrap()
    else {
        panic!("batch proof required")
    };
    refreshes
}
/// Exercise the public indexed dispatcher with its own writer lifetime. The
/// temporary vault, selected Catalog and assertion handles survive the call;
/// releasing the fixture writer is required for the application's writer lock.
fn batch_public_apply(
    fixture: Fixture,
    change: &crate::changes::PreparedChange,
) -> (Fixture, Result<crate::changes::ApplyReport>) {
    let Fixture {
        _temp,
        fs: fs_handle,
        writer,
        catalog,
        source,
        first,
    } = fixture;
    drop(writer);
    let app = crate::app::OfflineApp::new(
        fs_handle.clone(),
        crate::app::OperationOptions {
            offline: true,
            lock_timeout_ms: 1000,
            ..Default::default()
        },
    )
    .unwrap();
    let result =
        app.changes_apply(change.change_id.clone())
            .map(|outcome| crate::changes::ApplyReport {
                change: outcome
                    .change
                    .expect("public exact apply reports its retained Change"),
                status: outcome
                    .status
                    .expect("public exact apply reports its terminal status"),
                snapshot: outcome.snapshot,
            });
    drop(app);
    let writer = WriterPermit::acquire(fs_handle.root(), Duration::from_secs(1)).unwrap();
    (
        Fixture {
            _temp,
            fs: fs_handle,
            writer,
            catalog,
            source,
            first,
        },
        result,
    )
}
fn batch_recover_exact_once(
    fixture: Fixture,
    proof: crate::changes::indexed_refresh::IndexedRefreshProof,
) -> Fixture {
    let engine = ChangeEngine::new(fixture.fs.clone()).unwrap();
    let (fixture, result) = batch_public_apply(fixture, &proof.change);
    let report = result.unwrap();
    assert_eq!(report.status, ChangeStatus::Committed);
    assert_eq!(report.snapshot, Some(proof.intended.clone()));
    let reader = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    assert_eq!(QueryCatalog::snapshot(&reader), &proof.intended);
    let source_files: BTreeMap<_, _> = batch_recovery_targets(&proof)
        .into_iter()
        .map(|target| {
            let row = reader.record(&target.source_id).unwrap().unwrap();
            assert_eq!(row.record.id(), &target.source_id);
            assert_eq!(
                row.record.string("wiki_current_revision"),
                Some(target.revision_id.as_str())
            );
            let retained = scan::list(&row.record, "wiki_revisions");
            assert!(retained.contains(&target.previous_revision_id.to_string()));
            assert!(retained.contains(&target.revision_id.to_string()));
            let absolute = fixture.fs.root().path().join(row.path.as_str());
            (
                absolute.clone(),
                (
                    fs::read(&absolute).unwrap(),
                    fs::metadata(absolute).unwrap().modified().unwrap(),
                ),
            )
        })
        .collect();
    assert_eq!(
        engine
            .indexed_refresh_terminal_outcome(&proof.change)
            .unwrap(),
        Some(report.clone())
    );
    let (fixture, repeated) = batch_public_apply(fixture, &proof.change);
    assert_eq!(repeated.unwrap(), report);
    for (path, (bytes, modified)) in source_files {
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(
            fs::metadata(path).unwrap().modified().unwrap(),
            modified,
            "repeat never rewrites a Source"
        );
    }
    assert_eq!(
        QueryCatalog::snapshot(
            &fixture
                .catalog
                .query_snapshot(QueryReadLimits::default())
                .unwrap()
        ),
        &proof.intended
    );
    let authority = crate::changes::operation_authority::load(
        &fixture.fs,
        &id("vault_projector"),
        crate::changes::operation_authority::Presence::Required,
    )
    .unwrap()
    .unwrap();
    assert!(
        authority.active().is_none(),
        "exact recovery releases active intent"
    );
    assert_eq!(authority.publication().epoch, proof.intended.generation);
    drop(reader);
    fixture.oracle();
    fixture
}

const BATCH_ORIGINAL_CUTS: [BatchRecoveryCut; 7] = [
    BatchRecoveryCut::RetainedBaseline,
    BatchRecoveryCut::ActiveIntent,
    BatchRecoveryCut::FirstAsset,
    BatchRecoveryCut::FirstSource,
    BatchRecoveryCut::FilesApplied,
    BatchRecoveryCut::SqlBeforeCommit,
    BatchRecoveryCut::SqlAfterCommit,
];
fn batch_cut_fixture(managed: bool) -> Fixture {
    let fixture = Fixture::with_setup(true, |fixture| {
        if managed {
            crate::storage::cleanup(
                &fixture.fs,
                &fixture.writer,
                &crate::storage::StorageOptions::default(),
            )
            .unwrap();
        }
    });
    assert_eq!(
        crate::storage::layout::active(fixture.fs.root()).unwrap(),
        managed
    );
    fixture
}
fn batch_assert_cut_layout(
    fixture: &Fixture,
    proof: &crate::changes::indexed_refresh::IndexedRefreshProof,
    managed: bool,
    cut: BatchRecoveryCut,
) {
    for logical in [
        crate::changes::indexed_refresh::baseline_path(&proof.change).unwrap(),
        super::super::source_refresh::delta_path(&proof.change).unwrap(),
    ] {
        let physical =
            crate::storage::layout::physical_relative(fixture.fs.root(), &logical).unwrap();
        assert_eq!(
            physical.as_str(),
            if managed {
                format!(".wiki/retained/{}", logical.as_str())
            } else {
                logical.to_string()
            }
        );
        assert!(fixture.fs.root().path().join(physical.as_str()).is_file());
        if managed {
            assert!(
                !fixture.fs.root().path().join(logical.as_str()).exists(),
                "managed proof uses its actual retained path"
            );
        }
    }
    for target in batch_recovery_targets(proof) {
        for logical in [
            path(&format!("sources/{}/source.md", target.source_id)),
            path(&format!(
                "sources/{}/revisions/{}/original.bin",
                target.source_id, target.revision_id
            )),
        ] {
            assert_eq!(
                crate::storage::layout::physical_relative(fixture.fs.root(), &logical).unwrap(),
                logical,
                "Source and immutable evidence paths stay ordinary visible files"
            );
        }
    }
    let engine = ChangeEngine::new(fixture.fs.clone()).unwrap();
    let (manifest, _) = engine
        .load_manifest_structure(&proof.change.change_id)
        .unwrap();
    eprintln!(
        "batch cut={} layout={} reached=true source_members={} operations={} guarded_paths={}",
        cut.name(),
        if managed { "managed" } else { "original" },
        batch_recovery_targets(proof).len(),
        manifest.operations.len(),
        proof.before.len()
    );
}
fn batch_assert_terminal_cut(
    fixture: &Fixture,
    proof: &crate::changes::indexed_refresh::IndexedRefreshProof,
) {
    let engine = ChangeEngine::new(fixture.fs.clone()).unwrap();
    let (manifest, hash) = engine
        .load_manifest_structure(&proof.change.change_id)
        .unwrap();
    let receipt = crate::changes::outcome::terminal_report(&fixture.fs, &manifest, &hash)
        .unwrap()
        .unwrap();
    assert_eq!(receipt.status, ChangeStatus::Committed);
    assert_eq!(receipt.snapshot, Some(proof.intended.clone()));
    let journal = crate::changes::journal::load_journal(&fixture.fs, &manifest, &hash).unwrap();
    assert_eq!(
        journal.status,
        Some(ChangeStatus::Indexed),
        "receipt is retained before terminal journal append"
    );
    let authority = crate::changes::operation_authority::load(
        &fixture.fs,
        &id("vault_projector"),
        crate::changes::operation_authority::Presence::Required,
    )
    .unwrap()
    .unwrap();
    assert_eq!(authority.active().unwrap().change, proof.change);
    assert_eq!(
        authority.active().unwrap().intended.epoch,
        proof.intended.generation
    );
    assert_eq!(
        authority.publication().epoch,
        proof.base.generation,
        "authority cleanup has not acknowledged publication"
    );
    let current = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    assert_eq!(
        QueryCatalog::snapshot(&current),
        &proof.intended,
        "SQL publication has committed"
    );
}
#[test]
fn refresh_batch_returned_error_recovery_hits_retention_intent_assets_pointers_and_sql() {
    batch_returned_error_cuts(false, &BATCH_ORIGINAL_CUTS);
}
#[test]
fn refresh_batch_managed_layout_returned_error_recovery_hits_each_reached_cut() {
    batch_returned_error_cuts(true, &BATCH_ORIGINAL_CUTS);
}
#[test]
fn refresh_batch_terminal_receipt_before_authority_cleanup_returned_error_both_layouts() {
    for managed in [false, true] {
        batch_returned_error_cuts(managed, &[BatchRecoveryCut::TerminalReceipt]);
    }
}
fn batch_returned_error_cuts(managed: bool, cuts: &[BatchRecoveryCut]) {
    use crate::changes::indexed_refresh::IndexedRefreshPhase;
    for &cut in cuts {
        let fixture = batch_cut_fixture(managed);
        let unknown = b"unindexed unrelated bytes survive bounded recovery";
        fixture.write("unknown.bin", unknown);
        let (held_reader, projected) = batch_project_two(&fixture);
        let old = batch_old_immutable_bytes(&fixture, &held_reader);
        let old_source = held_reader.record(&fixture.source).unwrap().unwrap();
        let control = batch_control(&fixture, cut, false);
        let faulty = batch_fault_catalog(&fixture, &control);
        let engine = ChangeEngine::new(faulty.fs().clone()).unwrap();
        let proof = if cut == BatchRecoveryCut::RetainedBaseline {
            let error = IndexedRefreshSession::prepare_write(&faulty, &fixture.writer, projected)
                .err()
                .unwrap();
            let change: crate::changes::PreparedChange =
                serde_json::from_value(error.details["change"].clone()).unwrap();
            engine.load_indexed_refresh_proof(&change).unwrap().unwrap()
        } else {
            let mut session =
                IndexedRefreshSession::prepare_write(&faulty, &fixture.writer, projected).unwrap();
            let proof = session.proof().clone();
            assert!(
                engine
                    .apply_indexed_refresh(&fixture.writer, &mut session)
                    .is_err(),
                "returned-error cut {cut:?}"
            );
            assert_eq!(
                session.phase(),
                if matches!(
                    cut,
                    BatchRecoveryCut::SqlAfterCommit | BatchRecoveryCut::TerminalReceipt
                ) {
                    IndexedRefreshPhase::AlreadyPublished
                } else {
                    IndexedRefreshPhase::AtBase
                }
            );
            proof
        };
        assert!(
            control.fired.load(std::sync::atomic::Ordering::SeqCst),
            "cut actually reached {cut:?}"
        );
        assert_eq!(
            held_reader.record(&fixture.source).unwrap(),
            Some(old_source)
        );
        batch_assert_preserved(&fixture, &old, unknown);
        if cut == BatchRecoveryCut::FirstSource {
            let targets = batch_recovery_targets(&proof);
            let after = targets
                .iter()
                .filter(|target| {
                    let bytes = fs::read(
                        fixture
                            .fs
                            .root()
                            .path()
                            .join(format!("sources/{}/source.md", target.source_id)),
                    )
                    .unwrap();
                    parse_note(&bytes)
                        .canonical
                        .unwrap()
                        .string("wiki_current_revision")
                        == Some(target.revision_id.as_str())
                })
                .count();
            assert_eq!(
                after, 1,
                "actual cut between the two Source pointer replacements"
            );
        }
        batch_assert_cut_layout(&fixture, &proof, managed, cut);
        if cut == BatchRecoveryCut::TerminalReceipt {
            batch_assert_terminal_cut(&fixture, &proof);
        }
        drop(held_reader);
        let fixture = batch_recover_exact_once(fixture, proof);
        batch_assert_preserved(&fixture, &old, unknown);
    }
}

#[cfg(unix)]
#[test]
#[ignore = "invoked by enabled batch-specific SIGKILL wrapper"]
fn refresh_batch_native_crash_child() {
    let root = VaultRoot::explicit(std::env::var_os("LWIKI_BATCH_CRASH_VAULT").unwrap()).unwrap();
    let expected_layout = std::env::var("LWIKI_BATCH_CRASH_LAYOUT").unwrap();
    assert_eq!(
        crate::storage::layout::active(&root).unwrap(),
        expected_layout == "managed"
    );
    let cut = BatchRecoveryCut::parse(&std::env::var("LWIKI_BATCH_CRASH_CUT").unwrap());
    let control = std::sync::Arc::new(BatchCutControl {
        cut,
        fired: std::sync::atomic::AtomicBool::new(false),
        marker: Some(root.path().join(".wiki/batch-crash-ready")),
        proof_file: root.path().join(".wiki/batch-crash-proof.json"),
    });
    let plain_fs = VaultFs::new(root.clone());
    let plain = Catalog::new(plain_fs.clone(), id("vault_projector"));
    let sources: Vec<RecordId> = serde_json::from_slice(
        &fs::read(root.path().join(".wiki/batch-crash-sources.json")).unwrap(),
    )
    .unwrap();
    let reader = plain.query_snapshot(QueryReadLimits::default()).unwrap();
    let store = SourceStore::new(plain_fs.clone());
    let plans = sources
        .iter()
        .zip([b"new batch first".as_slice(), b"new batch other".as_slice()])
        .map(|(source, bytes)| {
            store
                .plan_refresh_indexed(
                    &reader,
                    source,
                    Fixture::request(bytes),
                    None,
                    &SourceRefreshLimits::default(),
                )
                .unwrap()
        })
        .collect();
    let projected = project_refresh_batch(
        &plain_fs,
        &reader,
        plans,
        &RefreshProjectionLimits::default(),
    )
    .unwrap()
    .unwrap();
    drop(reader);
    let faulty_fs = VaultFs::with_io(
        root.clone(),
        std::sync::Arc::new(BatchCutIo {
            control: control.clone(),
            journal_armed: std::sync::atomic::AtomicBool::new(false),
            terminal_parent: std::sync::Mutex::new(None),
        }),
    );
    let faulty = Catalog::with_options(
        faulty_fs.clone(),
        id("vault_projector"),
        crate::catalog::CatalogOptions {
            busy_timeout_ms: 1000,
            fault: Some(std::sync::Arc::new(BatchSqlCut(control.clone()))),
        },
    );
    let writer = WriterPermit::acquire(&root, Duration::from_secs(1)).unwrap();
    let mut session = IndexedRefreshSession::prepare_write(&faulty, &writer, projected).unwrap();
    fs::write(
        &control.proof_file,
        serde_json::to_vec(session.proof()).unwrap(),
    )
    .unwrap();
    ChangeEngine::new(faulty_fs)
        .unwrap()
        .apply_indexed_refresh(&writer, &mut session)
        .unwrap();
    panic!("native batch cut {cut:?} was not reached");
}

#[cfg(unix)]
#[test]
fn refresh_batch_native_sigkill_recovers_each_reached_cut_exactly_once() {
    batch_native_sigkill_cuts(false, &BATCH_ORIGINAL_CUTS);
}
#[cfg(unix)]
#[test]
fn refresh_batch_managed_layout_native_sigkill_recovers_each_reached_cut() {
    batch_native_sigkill_cuts(true, &BATCH_ORIGINAL_CUTS);
}
#[cfg(unix)]
#[test]
fn refresh_batch_terminal_receipt_before_authority_cleanup_native_sigkill_both_layouts() {
    for managed in [false, true] {
        batch_native_sigkill_cuts(managed, &[BatchRecoveryCut::TerminalReceipt]);
    }
}
#[cfg(unix)]
fn batch_native_sigkill_cuts(managed: bool, cuts: &[BatchRecoveryCut]) {
    use std::os::unix::process::ExitStatusExt;
    for &cut in cuts {
        let fixture = batch_cut_fixture(managed);
        let other = batch_other_source(&fixture);
        let unknown = b"native batch kill preserves unrelated unknown bytes";
        fixture.write("unknown.bin", unknown);
        fs::write(
            fixture
                .fs
                .root()
                .path()
                .join(".wiki/batch-crash-sources.json"),
            serde_json::to_vec(&[fixture.source.clone(), other]).unwrap(),
        )
        .unwrap();
        let held_reader = fixture
            .catalog
            .query_snapshot(QueryReadLimits::default())
            .unwrap();
        let old = batch_old_immutable_bytes(&fixture, &held_reader);
        let old_source = held_reader.record(&fixture.source).unwrap().unwrap();
        let Fixture {
            _temp,
            fs: fs_handle,
            writer,
            catalog,
            source,
            first,
        } = fixture;
        drop(writer);
        let child_name = format!(
            "{}::refresh_batch_native_crash_child",
            module_path!().split_once("::").unwrap().1
        );
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &child_name, "--ignored", "--nocapture"])
            .env("LWIKI_BATCH_CRASH_VAULT", fs_handle.root().path())
            .env("LWIKI_BATCH_CRASH_CUT", cut.name())
            .env(
                "LWIKI_BATCH_CRASH_LAYOUT",
                if managed { "managed" } else { "original" },
            )
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let marker = fs_handle.root().path().join(".wiki/batch-crash-ready");
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        // fs::write first creates the marker and then writes its contents.
        // Existence alone can expose an empty file while the reached callback
        // is still publishing its marker. Observe the complete expected bytes
        // before killing; the callback never rewrites this exact-cut marker.
        loop {
            if fs::read(&marker).is_ok_and(|bytes| bytes == cut.name().as_bytes()) {
                break;
            }
            if let Some(status) = child.try_wait().unwrap() {
                panic!("native batch {cut:?}: child exited before reaching cut: {status}");
            }
            if std::time::Instant::now() >= deadline {
                let observed = fs::read(&marker);
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("native batch {cut:?}: complete cut marker timeout: {observed:?}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(fs::read(&marker).unwrap(), cut.name().as_bytes());
        child.kill().unwrap();
        assert_eq!(
            child.wait().unwrap().signal(),
            Some(9),
            "actual native SIGKILL at {cut:?}"
        );
        let proof: crate::changes::indexed_refresh::IndexedRefreshProof = serde_json::from_slice(
            &fs::read(fs_handle.root().path().join(".wiki/batch-crash-proof.json")).unwrap(),
        )
        .unwrap();
        let writer = WriterPermit::acquire(fs_handle.root(), Duration::from_secs(1)).unwrap();
        let fixture = Fixture {
            _temp,
            fs: fs_handle,
            writer,
            catalog,
            source,
            first,
        };
        assert_eq!(
            held_reader.record(&fixture.source).unwrap(),
            Some(old_source),
            "held reader keeps original publication across {cut:?}"
        );
        batch_assert_preserved(&fixture, &old, unknown);
        if cut == BatchRecoveryCut::FirstSource {
            let changed = batch_recovery_targets(&proof)
                .iter()
                .filter(|target| {
                    let bytes = fs::read(
                        fixture
                            .fs
                            .root()
                            .path()
                            .join(format!("sources/{}/source.md", target.source_id)),
                    )
                    .unwrap();
                    parse_note(&bytes)
                        .canonical
                        .unwrap()
                        .string("wiki_current_revision")
                        == Some(target.revision_id.as_str())
                })
                .count();
            assert_eq!(
                changed, 1,
                "native kill is between two Source pointer replacements"
            );
        }
        batch_assert_cut_layout(&fixture, &proof, managed, cut);
        if cut == BatchRecoveryCut::TerminalReceipt {
            batch_assert_terminal_cut(&fixture, &proof);
        }
        drop(held_reader);
        let fixture = batch_recover_exact_once(fixture, proof);
        batch_assert_preserved(&fixture, &old, unknown);
    }
}

#[test]
fn refresh_batch_wal_failure_is_postcommit_and_held_reader_defers_maintenance() {
    use crate::changes::indexed_refresh::IndexedRefreshPhase;
    for injected in [false, true] {
        let fixture = Fixture::new(true);
        let (held_reader, projected) = batch_project_two(&fixture);
        let old_source = held_reader.record(&fixture.source).unwrap().unwrap();
        let old = batch_old_immutable_bytes(&fixture, &held_reader);
        let engine = ChangeEngine::new(fixture.fs.clone()).unwrap();
        let mut session =
            IndexedRefreshSession::prepare_write(&fixture.catalog, &fixture.writer, projected)
                .unwrap();
        let proof = session.proof().clone();
        if injected {
            assert_eq!(
                selector::checkpoint_error(Some(rusqlite::ffi::SQLITE_IOERR)),
                None
            );
            let result = engine.apply_indexed_refresh(&fixture.writer, &mut session);
            selector::checkpoint_error(None);
            let error = result.unwrap_err();
            assert_eq!(error.details["publication_committed"], json!(true));
            assert_eq!(
                error.details["maintenance"],
                json!("wal_checkpoint_truncate")
            );
            assert_eq!(session.phase(), IndexedRefreshPhase::AlreadyPublished);
        } else {
            // A real external read transaction keeps the old SQL epoch open.
            // Busy TRUNCATE is a normal deferred result, not an injected error.
            assert_eq!(
                engine
                    .apply_indexed_refresh(&fixture.writer, &mut session)
                    .unwrap()
                    .status,
                ChangeStatus::Committed
            );
        }
        assert_eq!(
            held_reader.record(&fixture.source).unwrap(),
            Some(old_source)
        );
        drop(session);
        drop(held_reader);
        let fixture = batch_recover_exact_once(fixture, proof);
        for (path, bytes) in old {
            assert_eq!(
                fs::read(fixture.fs.root().path().join(path.as_str())).unwrap(),
                bytes
            );
        }
    }
}

#[test]
fn refresh_batch_retained_tampering_with_recomputed_checksums_cannot_forge_source_metadata() {
    use crate::changes::indexed_refresh::{IndexedWriteOperation, baseline_path};
    for tamper in [
        "noop_blob_missing",
        "noop_blob_extra",
        "noop_blob_changed",
        "noop_source_row_under_hash",
        "changed_source_row_under_hash",
        "source_equals_other_revision",
    ] {
        let fixture = Fixture::new(true);
        let other = batch_other_source(&fixture);
        let (reader, plans) = batch_plans(
            &fixture,
            &[
                (&fixture.source, b"first capture quote", None),
                (&other, b"changed other", None),
            ],
        );
        let projected = project_refresh_batch(
            &fixture.fs,
            &reader,
            plans,
            &RefreshProjectionLimits::default(),
        )
        .unwrap()
        .unwrap();
        let session =
            IndexedRefreshSession::prepare_write(&fixture.catalog, &fixture.writer, projected)
                .unwrap();
        let mut proof = session.proof().clone();
        drop(session);
        let source_paths =
            [&fixture.source, &other].map(|source| reader.record(source).unwrap().unwrap().path);
        let source_before = source_paths
            .clone()
            .map(|path| fs::read(fixture.fs.root().path().join(path.as_str())).unwrap());
        let retained_path = super::super::source_refresh::delta_path(&proof.change).unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(
            &fs::read(fixture.fs.root().path().join(retained_path.as_str())).unwrap(),
        )
        .unwrap();
        let mut operation: IndexedWriteOperation =
            serde_json::from_value(value["operation"].clone()).unwrap();
        let mut rows: CatalogDelta = serde_json::from_value(value["rows"].clone()).unwrap();
        let IndexedWriteOperation::SourceRefreshBatch { refreshes, .. } = &mut operation else {
            panic!("batch operation");
        };
        let noop = refreshes.iter().position(|target| target.no_op).unwrap();
        let changed = refreshes.iter().position(|target| !target.no_op).unwrap();
        match tamper {
            "noop_blob_missing" => refreshes[noop].unchanged_source = None,
            "noop_blob_extra" => {
                refreshes[changed].unchanged_source = refreshes[noop].unchanged_source.clone()
            }
            "noop_blob_changed" => refreshes[noop]
                .unchanged_source
                .as_mut()
                .unwrap()
                .push_str("forged body"),
            "noop_source_row_under_hash" | "changed_source_row_under_hash" => {
                let source_id = if tamper == "noop_source_row_under_hash" {
                    &fixture.source
                } else {
                    &other
                };
                let row = rows
                    .records
                    .iter_mut()
                    .find(|row| row.record.id() == source_id)
                    .unwrap();
                let claimed_hash = row.hash.clone();
                let mut fields = row.record.fields().clone();
                fields.insert(
                    "title".into(),
                    json!("forged canonical title under unchanged claimed hash"),
                );
                row.record = CanonicalRecord::new(fields).unwrap();
                assert_eq!(row.hash, claimed_hash);
            }
            "source_equals_other_revision" => {
                refreshes[noop].source_id = refreshes[changed].revision_id.clone();
                refreshes.sort_by(|left, right| left.source_id.cmp(&right.source_id));
            }
            _ => unreachable!(),
        }
        value["operation"] = serde_json::to_value(&operation).unwrap();
        value["rows"] = serde_json::to_value(rows).unwrap();
        let bytes = serde_json::to_vec(&value).unwrap();
        proof.operation = Some(operation);
        proof.delta_hash = Blake3Hash::digest(&bytes);
        let publication_hash = Blake3Hash::digest(
            serde_json::to_vec(&(
                "lwiki.normalized-write-publication.v1",
                &proof.base,
                &proof.change,
                &proof.delta_hash,
            ))
            .unwrap(),
        );
        proof.intended = ReadSnapshot::published(
            proof.base.generation + 1,
            proof.base.parser_fingerprint.clone(),
            proof.base.publication().unwrap().file_id.clone(),
            publication_hash,
        )
        .unwrap();
        fixture.write(retained_path.as_str(), &bytes);
        // This intentionally recomputes both checksums. The rejection must
        // establish metadata/ownership validation, beyond accidental corruption.
        let checksum = Blake3Hash::digest(serde_json::to_vec(&proof).unwrap());
        fixture.write(
            baseline_path(&proof.change).unwrap().as_str(),
            &serde_json::to_vec(&json!({
                "proof":proof, "checksum":checksum,
            }))
            .unwrap(),
        );
        assert!(
            IndexedRefreshSession::resume(&fixture.catalog, &fixture.writer, proof).is_err(),
            "retained forged plan accepted {tamper}"
        );
        for (path, bytes) in source_paths.into_iter().zip(source_before) {
            assert_eq!(
                fs::read(fixture.fs.root().path().join(path.as_str())).unwrap(),
                bytes,
                "no canonical mutation for {tamper}"
            );
        }
    }
}

#[test]
fn refresh_batch_global_source_revision_disjointness_precedes_admission() {
    let fixture = Fixture::new(true);
    let other = batch_other_source(&fixture);
    let (reader, mut plans) = batch_plans(
        &fixture,
        &[
            (&fixture.source, b"new first", None),
            (&other, b"new other", None),
        ],
    );
    plans[0].plan.source_id = plans[1].previous_revision.clone();
    let error = project_refresh_batch(
        &fixture.fs,
        &reader,
        plans,
        &RefreshProjectionLimits::default(),
    )
    .err()
    .unwrap();
    assert_eq!(error.code, ErrorCode::ContentConflict);
    assert!(error.message.contains("ownership overlaps"));
}

#[test]
fn refresh_batch_staged_base_drift_and_missing_or_damaged_evidence_refuse_before_writes() {
    use crate::changes::indexed_refresh::baseline_path;
    for damage in [
        "stale_base",
        "missing_delta",
        "damaged_delta",
        "missing_baseline",
        "damaged_baseline",
        "missing_payload",
        "damaged_payload",
    ] {
        let fixture = Fixture::new(true);
        let (reader, projected) = batch_project_two(&fixture);
        let session =
            IndexedRefreshSession::prepare_write(&fixture.catalog, &fixture.writer, projected)
                .unwrap();
        let proof = session.proof().clone();
        drop(session);
        drop(reader);
        let engine = ChangeEngine::new(fixture.fs.clone()).unwrap();
        let (manifest, _) = engine.load_manifest(&proof.change.change_id).unwrap();
        match damage {
            "stale_base" => {
                fixture
                    .apply(
                        Fixture::request(b"first capture quote"),
                        Some("independent epoch changes staged base"),
                    )
                    .unwrap();
            }
            "missing_delta" => {
                fs::remove_file(
                    fixture.fs.root().path().join(
                        super::super::source_refresh::delta_path(&proof.change)
                            .unwrap()
                            .as_str(),
                    ),
                )
                .unwrap();
            }
            "damaged_delta" => fixture.write(
                super::super::source_refresh::delta_path(&proof.change)
                    .unwrap()
                    .as_str(),
                b"damaged retained delta",
            ),
            "missing_baseline" => {
                fs::remove_file(
                    fixture
                        .fs
                        .root()
                        .path()
                        .join(baseline_path(&proof.change).unwrap().as_str()),
                )
                .unwrap();
            }
            "damaged_baseline" => fixture.write(
                baseline_path(&proof.change).unwrap().as_str(),
                b"damaged retained proof",
            ),
            "missing_payload" | "damaged_payload" => {
                let operation = manifest
                    .operations
                    .iter()
                    .find(|operation| operation.target.as_str().ends_with("/content.md"))
                    .unwrap();
                let payload = operation.after_payload.as_ref().unwrap();
                if damage == "missing_payload" {
                    fs::remove_file(fixture.fs.root().path().join(payload.path.as_str())).unwrap();
                } else {
                    fixture.write(payload.path.as_str(), b"damaged exact payload");
                }
            }
            _ => unreachable!(),
        }
        let targets = batch_recovery_targets(&proof);
        let before: Vec<_> = targets
            .iter()
            .map(|target| {
                let path = format!("sources/{}/source.md", target.source_id);
                (
                    path.clone(),
                    fs::read(fixture.fs.root().path().join(path)).unwrap(),
                )
            })
            .collect();
        // Ordinary dispatch authenticates the retained baseline before resuming.
        let refused = match engine.load_indexed_refresh_proof(&proof.change) {
            Err(_) | Ok(None) => true,
            Ok(Some(retained)) => {
                match IndexedRefreshSession::resume(&fixture.catalog, &fixture.writer, retained) {
                    Err(_) => true,
                    Ok(mut session) => engine
                        .apply_indexed_refresh(&fixture.writer, &mut session)
                        .is_err(),
                }
            }
        };
        assert!(refused, "staged guard {damage}");
        for (path, bytes) in before {
            assert_eq!(
                fs::read(fixture.fs.root().path().join(path)).unwrap(),
                bytes,
                "refusal preserves Source for {damage}"
            );
        }
    }
}

#[test]
fn refresh_batch_recovery_third_hash_in_pointer_asset_or_noop_guard_preserves_foreign_bytes() {
    for managed in [false, true] {
        for changed in ["source", "asset", "noop"] {
            let fixture = batch_cut_fixture(managed);
            let other = batch_other_source(&fixture);
            let (reader, plans) = batch_plans(
                &fixture,
                &[
                    (&fixture.source, b"first capture quote", None),
                    (&other, b"new batch other", None),
                ],
            );
            let projected = project_refresh_batch(
                &fixture.fs,
                &reader,
                plans,
                &RefreshProjectionLimits::default(),
            )
            .unwrap()
            .unwrap();
            let control = batch_control(&fixture, BatchRecoveryCut::FirstAsset, false);
            let faulty = batch_fault_catalog(&fixture, &control);
            let mut session =
                IndexedRefreshSession::prepare_write(&faulty, &fixture.writer, projected).unwrap();
            let proof = session.proof().clone();
            let faulty_engine = ChangeEngine::new(faulty.fs().clone()).unwrap();
            assert!(
                faulty_engine
                    .apply_indexed_refresh(&fixture.writer, &mut session)
                    .is_err()
            );
            assert!(control.fired.load(std::sync::atomic::Ordering::SeqCst));
            drop(session);
            let engine = ChangeEngine::new(fixture.fs.clone()).unwrap();
            let (manifest, _) = engine.load_manifest(&proof.change.change_id).unwrap();
            let foreign_path = match changed {
                "source" => format!("sources/{other}/source.md"),
                "noop" => format!("sources/{}/source.md", fixture.source),
                "asset" => manifest
                    .operations
                    .iter()
                    .find(|operation| operation.target.as_str().ends_with("/original.bin"))
                    .unwrap()
                    .target
                    .to_string(),
                _ => unreachable!(),
            };
            let foreign = b"unknown third hash must survive refusal";
            fixture.write(&foreign_path, foreign);
            let pointers: Vec<_> = [&fixture.source, &other]
                .into_iter()
                .map(|source| {
                    let path = format!("sources/{source}/source.md");
                    (
                        path.clone(),
                        fs::read(fixture.fs.root().path().join(path)).unwrap(),
                    )
                })
                .collect();
            drop(reader);
            let (fixture, result) = batch_public_apply(fixture, &proof.change);
            let error = result
                .err()
                .expect("public indexed third-hash replay must refuse");
            assert!(
                matches!(
                    error.code,
                    ErrorCode::RecoveryRequired
                        | ErrorCode::ContentConflict
                        | ErrorCode::FreshnessConflict
                        | ErrorCode::IndexCorrupt
                        | ErrorCode::SourceIntegrity
                ),
                "indexed third-hash guard {changed}, managed={managed}: {error:?}"
            );
            assert_eq!(
                fs::read(fixture.fs.root().path().join(foreign_path)).unwrap(),
                foreign
            );
            for (path, bytes) in pointers {
                assert_eq!(
                    fs::read(fixture.fs.root().path().join(path)).unwrap(),
                    bytes
                );
            }
        }
    }
}

/// Recompute the entire retained envelope after changing row actions. This is an
/// adversarial internal-plan test: valid checksums alone confer no byte authority.
fn batch_rewrite_retained_rows(
    fixture: &Fixture,
    mut proof: crate::changes::indexed_refresh::IndexedRefreshProof,
    rows: CatalogDelta,
) -> crate::changes::indexed_refresh::IndexedRefreshProof {
    // Resume checks the retained delta as well as this envelope. Some controls
    // intentionally violate an existing policy byte binding; others preserve
    // generic shape and reach the batch's exact document/ownership checks.
    let path = super::super::source_refresh::delta_path(&proof.change).unwrap();
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.fs.root().path().join(path.as_str())).unwrap())
            .unwrap();
    value["rows"] = serde_json::to_value(rows).unwrap();
    value["change"] = serde_json::to_value(&proof.change).unwrap();
    value["operation"] = serde_json::to_value(&proof.operation).unwrap();
    let bytes = serde_json::to_vec(&value).unwrap();
    proof.delta_hash = Blake3Hash::digest(&bytes);
    let publication_hash = Blake3Hash::digest(
        serde_json::to_vec(&(
            "lwiki.normalized-write-publication.v1",
            &proof.base,
            &proof.change,
            &proof.delta_hash,
        ))
        .unwrap(),
    );
    proof.intended = ReadSnapshot::published(
        proof.base.generation + 1,
        proof.base.parser_fingerprint.clone(),
        proof.base.publication().unwrap().file_id.clone(),
        publication_hash,
    )
    .unwrap();
    fixture.write(path.as_str(), &bytes);
    let checksum = Blake3Hash::digest(serde_json::to_vec(&proof).unwrap());
    fixture.write(
        crate::changes::indexed_refresh::baseline_path(&proof.change)
            .unwrap()
            .as_str(),
        &serde_json::to_vec(&json!({"proof":&proof,"checksum":checksum})).unwrap(),
    );
    let engine = ChangeEngine::new(fixture.fs.clone()).unwrap();
    if proof.operation.as_ref().unwrap().validate().is_ok() {
        assert_eq!(
            engine.load_indexed_refresh_proof(&proof.change).unwrap(),
            Some(proof.clone()),
            "forged row actions have valid envelope checksums before semantic rejection"
        );
    } else {
        assert!(
            engine.load_indexed_refresh_proof(&proof.change).is_err(),
            "checksummed malformed descriptor is rejected by semantic validation"
        );
    }
    proof
}

fn batch_retained_rows(
    fixture: &Fixture,
    proof: &crate::changes::indexed_refresh::IndexedRefreshProof,
) -> CatalogDelta {
    let value: serde_json::Value = serde_json::from_slice(
        &fs::read(
            fixture.fs.root().path().join(
                super::super::source_refresh::delta_path(&proof.change)
                    .unwrap()
                    .as_str(),
            ),
        )
        .unwrap(),
    )
    .unwrap();
    serde_json::from_value(value["rows"].clone()).unwrap()
}

fn batch_external_source_fixture() -> Fixture {
    Fixture::with_setup(true, |fixture| {
        let other = batch_other_source(fixture);
        let name = format!("sources/{other}/source.md");
        let parsed = parse_note(&fs::read(fixture.fs.root().path().join(&name)).unwrap());
        let bytes = crate::records::edit_note(
            &parsed,
            &BTreeMap::from([("wiki_depends_on_ids".into(), json!(["assertion_opposite"]))]),
            None,
            &parsed.source_hash,
        )
        .unwrap();
        fixture.write(&name, &bytes);
    })
}

#[test]
fn refresh_batch_external_source_dependency_preserves_bytes_and_historical_completion() {
    let fixture = batch_external_source_fixture();
    let other = batch_other_source(&fixture);
    let name = format!("sources/{other}/source.md");
    let original = fs::read(fixture.fs.root().path().join(&name)).unwrap();
    let (delta, change) = batch_apply(
        &fixture,
        &[(&fixture.source, b"fresh selected member", None)],
    );
    assert_eq!(
        delta
            .records
            .iter()
            .find(|row| row.record.id() == &other)
            .unwrap()
            .eligibility,
        Eligibility::Stale
    );
    assert_eq!(
        fs::read(fixture.fs.root().path().join(&name)).unwrap(),
        original
    );
    fixture.oracle();
    let engine = ChangeEngine::new(fixture.fs.clone()).unwrap();
    let proof = engine.load_indexed_refresh_proof(&change).unwrap().unwrap();
    let crate::changes::indexed_refresh::IndexedWriteOperation::SourceRefreshBatch {
        dependent_sources,
        ..
    } = proof.operation.as_ref().unwrap()
    else {
        panic!("batch descriptor");
    };
    assert_eq!(dependent_sources.len(), 1);
    assert_eq!(dependent_sources[0].source_id, other);
    assert_eq!(dependent_sources[0].source_bytes.as_bytes(), original);
    batch_apply(&fixture, &[(&fixture.source, b"first capture quote", None)]);
    fixture.oracle();
    let reader = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    assert_eq!(
        reader.record(&other).unwrap().unwrap().eligibility,
        Eligibility::Current
    );
    let publication = QueryCatalog::snapshot(&reader).clone();
    drop(reader);
    let mut edited = original.clone();
    edited.extend_from_slice(b"\nLater authored Source note\n");
    fixture.write(&name, &edited);
    let report = engine
        .apply(
            &fixture.writer,
            &change,
            &crate::catalog::CatalogGraphValidator,
            &fixture.catalog,
        )
        .unwrap();
    assert_eq!(report.status, ChangeStatus::Committed);
    assert_eq!(report.snapshot, Some(proof.intended));
    assert_eq!(
        fs::read(fixture.fs.root().path().join(&name)).unwrap(),
        edited
    );
    let reader = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    assert_eq!(QueryCatalog::snapshot(&reader), &publication);
}

#[test]
fn refresh_batch_external_source_witness_and_rows_cannot_be_forged() {
    use crate::changes::indexed_refresh::{IndexedRefreshSourceDependency, IndexedWriteOperation};
    for tamper in [
        "row",
        "missing",
        "extra",
        "bytes",
        "member_overlap",
        "revision_overlap",
    ] {
        let fixture = batch_external_source_fixture();
        let other = batch_other_source(&fixture);
        let (reader, plans) = batch_plans(
            &fixture,
            &[(&fixture.source, b"fresh selected member", None)],
        );
        let projected = project_refresh_batch(
            &fixture.fs,
            &reader,
            plans,
            &RefreshProjectionLimits::default(),
        )
        .unwrap()
        .unwrap();
        let session =
            IndexedRefreshSession::prepare_write(&fixture.catalog, &fixture.writer, projected)
                .unwrap();
        let mut proof = session.proof().clone();
        drop(session);
        let before = batch_member_source_bytes(&fixture, &proof);
        let external_path = format!("sources/{other}/source.md");
        let original = fs::read(fixture.fs.root().path().join(&external_path)).unwrap();
        let mut rows = batch_retained_rows(&fixture, &proof);
        let IndexedWriteOperation::SourceRefreshBatch {
            refreshes,
            dependent_sources,
        } = proof.operation.as_mut().unwrap()
        else {
            panic!("batch descriptor");
        };
        match tamper {
            "row" => {
                let row = rows
                    .records
                    .iter_mut()
                    .find(|row| row.record.id() == &other)
                    .unwrap();
                let mut fields = row.record.fields().clone();
                fields.insert("title".into(), json!("forged external title"));
                row.record = CanonicalRecord::new(fields).unwrap();
            }
            "missing" => dependent_sources.clear(),
            "extra" => {
                dependent_sources.push(IndexedRefreshSourceDependency {
                    source_id: id("source_unrelated"),
                    source_bytes: String::from_utf8(original.clone()).unwrap(),
                });
                dependent_sources.sort_by(|left, right| left.source_id.cmp(&right.source_id));
            }
            "bytes" => dependent_sources[0].source_bytes.push_str("forged bytes"),
            "member_overlap" => dependent_sources[0].source_id = refreshes[0].source_id.clone(),
            "revision_overlap" => {
                dependent_sources[0].source_id = refreshes[0].previous_revision_id.clone()
            }
            _ => unreachable!(),
        }
        if tamper.ends_with("overlap") {
            assert!(proof.operation.as_ref().unwrap().validate().is_err());
        } else {
            let proof = batch_rewrite_retained_rows(&fixture, proof, rows);
            assert!(
                IndexedRefreshSession::resume(&fixture.catalog, &fixture.writer, proof).is_err(),
                "forgery {tamper}"
            );
        }
        batch_assert_source_bytes(&fixture, &before);
        assert_eq!(
            fs::read(fixture.fs.root().path().join(&external_path)).unwrap(),
            original
        );
    }
}

fn batch_member_source_bytes(
    fixture: &Fixture,
    proof: &crate::changes::indexed_refresh::IndexedRefreshProof,
) -> BTreeMap<VaultRelativePath, Vec<u8>> {
    batch_recovery_targets(proof)
        .iter()
        .map(|target| {
            let path = path(&format!("sources/{}/source.md", target.source_id));
            (
                path.clone(),
                fs::read(fixture.fs.root().path().join(path.as_str())).unwrap(),
            )
        })
        .collect()
}

fn batch_assert_source_bytes(fixture: &Fixture, expected: &BTreeMap<VaultRelativePath, Vec<u8>>) {
    for (path, bytes) in expected {
        assert_eq!(
            fs::read(fixture.fs.root().path().join(path.as_str())).unwrap(),
            *bytes,
            "forged retained actions never mutate Source {path}"
        );
    }
}

#[test]
fn refresh_batch_rechecksummed_document_put_text_and_ownership_forgery_refuses() {
    for tamper in [
        "source_body",
        "source_raw",
        "source_title",
        "revision_body",
        "revision_raw",
        "revision_title",
        "content_body",
        "content_raw",
        "content_title",
        "content_source_owner",
        "content_revision_owner",
    ] {
        let fixture = Fixture::new(true);
        let other = batch_other_source(&fixture);
        let (reader, plans) = batch_plans(
            &fixture,
            &[
                (&fixture.source, b"first capture quote", None),
                (
                    &other,
                    b"# Fresh retained content\n\nactual captured statement",
                    None,
                ),
            ],
        );
        let projected = project_refresh_batch(
            &fixture.fs,
            &reader,
            plans,
            &RefreshProjectionLimits::default(),
        )
        .unwrap()
        .unwrap();
        let session =
            IndexedRefreshSession::prepare_write(&fixture.catalog, &fixture.writer, projected)
                .unwrap();
        let proof = session.proof().clone();
        drop(session);
        let before = batch_member_source_bytes(&fixture, &proof);
        let old = batch_old_immutable_bytes(&fixture, &reader);
        fixture.write(
            "unknown.bin",
            b"unowned bytes remain intact during document forgery refusal",
        );
        let mut rows = batch_retained_rows(&fixture, &proof);
        let fresh = batch_recovery_targets(&proof)
            .iter()
            .find(|target| !target.reused)
            .unwrap();
        let target_path = if tamper.starts_with("source_") {
            path(&format!("sources/{}/source.md", fresh.source_id))
        } else if tamper.starts_with("revision_") {
            path(&format!(
                "sources/{}/revisions/{}/revision.md",
                fresh.source_id, fresh.revision_id
            ))
        } else {
            path(&format!(
                "sources/{}/revisions/{}/content.md",
                fresh.source_id, fresh.revision_id
            ))
        };
        let row = rows
            .documents
            .iter_mut()
            .find_map(|mutation| match mutation {
                DocumentMutation::Put { row } if row.path == target_path => Some(row),
                _ => None,
            })
            .unwrap();
        let hash = row.hash.clone();
        match tamper {
            "source_body" | "revision_body" | "content_body" => {
                row.body.push_str(" forged indexed evidence")
            }
            "source_raw" | "revision_raw" | "content_raw" => {
                row.raw_text.push_str("\nforged raw bytes")
            }
            "source_title" | "revision_title" | "content_title" => {
                row.title = "forged search title".into()
            }
            "content_source_owner" => row.source_id = Some(fixture.source.clone()),
            "content_revision_owner" => row.owner_revision = Some(fixture.first.clone()),
            _ => unreachable!(),
        }
        assert_eq!(
            row.hash, hash,
            "claimed hash unchanged while derived content differs"
        );
        let forged = batch_rewrite_retained_rows(&fixture, proof, rows);
        let error = IndexedRefreshSession::resume(&fixture.catalog, &fixture.writer, forged)
            .err()
            .unwrap();
        assert!(
            error.message.contains("document rows differ")
                || error
                    .message
                    .contains("policy membership differs from document bytes or classification")
                || error
                    .message
                    .contains("policy membership lacks exact document replacement")
                || error.message.contains(
                    "normalized canonical writes require exact policy membership replacements"
                ),
            "{tamper}: {:?}",
            error
        );
        batch_assert_source_bytes(&fixture, &before);
        batch_assert_preserved(
            &fixture,
            &old,
            b"unowned bytes remain intact during document forgery refusal",
        );
    }
}

#[test]
fn refresh_batch_rechecksummed_missing_or_extra_member_document_put_refuses() {
    for tamper in [
        "missing_source",
        "missing_revision",
        "missing_content",
        "extra_noop_source",
        "extra_old_revision",
    ] {
        let fixture = Fixture::new(true);
        let other = batch_other_source(&fixture);
        let (reader, plans) = batch_plans(
            &fixture,
            &[
                (&fixture.source, b"first capture quote", None),
                (&other, b"fresh batch content", None),
            ],
        );
        let projected = project_refresh_batch(
            &fixture.fs,
            &reader,
            plans,
            &RefreshProjectionLimits::default(),
        )
        .unwrap()
        .unwrap();
        let session =
            IndexedRefreshSession::prepare_write(&fixture.catalog, &fixture.writer, projected)
                .unwrap();
        let proof = session.proof().clone();
        drop(session);
        let before = batch_member_source_bytes(&fixture, &proof);
        let mut rows = batch_retained_rows(&fixture, &proof);
        let fresh = batch_recovery_targets(&proof)
            .iter()
            .find(|target| !target.reused)
            .unwrap();
        if tamper.starts_with("missing_") {
            let name = match tamper {
                "missing_source" => "source.md",
                "missing_revision" => "revision.md",
                _ => "content.md",
            };
            let target = if name == "source.md" {
                path(&format!("sources/{}/source.md", fresh.source_id))
            } else {
                path(&format!(
                    "sources/{}/revisions/{}/{name}",
                    fresh.source_id, fresh.revision_id
                ))
            };
            let index = rows
                .documents
                .iter()
                .position(|mutation| {
                    matches!(mutation,
                DocumentMutation::Put { row } if row.path == target)
                })
                .unwrap();
            rows.documents.remove(index);
        } else {
            let id = if tamper == "extra_noop_source" {
                &fixture.source
            } else {
                &fresh.previous_revision_id
            };
            let row = if tamper == "extra_noop_source" {
                reader.record(id).unwrap().unwrap()
            } else {
                rows.records
                    .iter()
                    .find(|row| row.record.id() == id)
                    .unwrap()
                    .clone()
            };
            let note =
                parse_note(&fs::read(fixture.fs.root().path().join(row.path.as_str())).unwrap());
            // Replace a legitimate old-head Metadata action rather than
            // adding a duplicate path, so generic delta validation still passes.
            rows.documents.retain(|mutation| {
                !matches!(mutation,
                DocumentMutation::Metadata { path, .. } if path == &row.path)
            });
            rows.documents.push(DocumentMutation::Put {
                row: row_projection::canonical_document(&row.path, &note, Some(&row)),
            });
        }
        let forged = batch_rewrite_retained_rows(&fixture, proof, rows);
        let error = IndexedRefreshSession::resume(&fixture.catalog, &fixture.writer, forged)
            .err()
            .unwrap();
        assert!(
            error.message.contains("document rows differ")
                || error
                    .message
                    .contains("policy membership differs from document bytes or classification")
                || error
                    .message
                    .contains("policy membership lacks exact document replacement")
                || error.message.contains(
                    "normalized canonical writes require exact policy membership replacements"
                ),
            "{tamper}: {:?}",
            error
        );
        batch_assert_source_bytes(&fixture, &before);
    }
}

#[test]
fn refresh_batch_rechecksummed_historical_head_revision_row_owner_path_and_title_refuse() {
    for head in ["old", "new"] {
        for tamper in ["wiki_source_id", "title", "path"] {
            let fixture = Fixture::new(true);
            let other = batch_other_source(&fixture);
            fixture
                .apply(
                    Fixture::request(b"second captured immutable first-source revision"),
                    None,
                )
                .unwrap();
            let (reader, plans) = batch_plans(
                &fixture,
                &[
                    (&fixture.source, b"first capture quote", None),
                    (&other, b"other capture quote", None),
                ],
            );
            assert!(plans.iter().all(|plan| plan.plan.reused));
            let projected = project_refresh_batch(
                &fixture.fs,
                &reader,
                plans,
                &RefreshProjectionLimits::default(),
            )
            .unwrap()
            .unwrap();
            let session =
                IndexedRefreshSession::prepare_write(&fixture.catalog, &fixture.writer, projected)
                    .unwrap();
            let proof = session.proof().clone();
            drop(session);
            let before = batch_member_source_bytes(&fixture, &proof);
            let old = batch_old_immutable_bytes(&fixture, &reader);
            fixture.write(
                "unknown.bin",
                b"historical member forgery never changes immutable files",
            );
            let mut rows = batch_retained_rows(&fixture, &proof);
            let historical = batch_recovery_targets(&proof)
                .iter()
                .find(|target| !target.no_op)
                .unwrap();
            assert!(historical.reused && historical.previous_revision_id != historical.revision_id);
            let revision = if head == "old" {
                &historical.previous_revision_id
            } else {
                &historical.revision_id
            };
            let row = rows
                .records
                .iter_mut()
                .find(|row| row.record.id() == revision)
                .unwrap();
            let hash = row.hash.clone();
            if tamper == "path" {
                row.path = path(&format!("sources/{other}/revisions/{revision}/revision.md"));
            } else {
                let mut fields = row.record.fields().clone();
                fields.insert(
                    tamper.into(),
                    if tamper == "title" {
                        json!("forged immutable title")
                    } else {
                        json!(other)
                    },
                );
                row.record = CanonicalRecord::new(fields).unwrap();
            }
            assert_eq!(
                row.hash, hash,
                "immutable row keeps the claimed selected hash"
            );
            let forged = batch_rewrite_retained_rows(&fixture, proof, rows);
            let error = IndexedRefreshSession::resume(&fixture.catalog, &fixture.writer, forged)
                .err()
                .unwrap();
            assert!(
                error
                    .message
                    .contains("retained Revision identity or owner differs"),
                "{head}/{tamper}: {:?}",
                error
            );
            batch_assert_source_bytes(&fixture, &before);
            batch_assert_preserved(
                &fixture,
                &old,
                b"historical member forgery never changes immutable files",
            );
        }
    }
}

// Frozen prospectively against operator/cli-acceptance-002.py. Only physical
// SQLite row IDs and publication/build bookkeeping are excluded; cache-unit
// state is compared in full (this fixture intentionally has no unit policy).
const BATCH_LOGICAL_TABLES: [&str; 23] = [
    "documents",
    "graph_rows",
    "records",
    "identity_claims",
    "source_revision_identity",
    "source_evidence",
    "revision_tree_owners",
    "links",
    "assertion_navigation_keys",
    "opposition_members",
    "record_eligibility_facts",
    "record_direct_paths",
    "semantic_edges",
    "link_facts",
    "link_match_keys",
    "registry_match_keys",
    "policy_facts",
    "diagnostics",
    "dependencies",
    "retrieval_units",
    "unit_owners",
    "unit_owner_dependencies",
    "unit_policies",
];
fn batch_sql_rows(connection: &rusqlite::Connection, sql: &str) -> Vec<Vec<String>> {
    let mut statement = connection.prepare(sql).unwrap();
    let columns = statement.column_count();
    let mut rows = statement.query([]).unwrap();
    let mut result = Vec::new();
    while let Some(row) = rows.next().unwrap() {
        result.push(
            (0..columns)
                .map(|column| format!("{:?}", row.get_ref(column).unwrap()))
                .collect(),
        );
    }
    result.sort();
    result
}
fn batch_complete_projection(
    connection: &rusqlite::Connection,
) -> BTreeMap<String, Vec<Vec<String>>> {
    let mut expected: BTreeSet<String> =
        BATCH_LOGICAL_TABLES.iter().map(|s| s.to_string()).collect();
    expected.insert("catalog_meta".into());
    for prefix in ["documents", "graph"] {
        for suffix in [
            "fts",
            "vocab",
            "fts_config",
            "fts_data",
            "fts_docsize",
            "fts_idx",
        ] {
            expected.insert(format!("{prefix}_{suffix}"));
        }
    }
    let tables: BTreeSet<String> = connection
        .prepare("SELECT name FROM sqlite_master WHERE type='table'")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    assert_eq!(
        tables, expected,
        "unknown/missing table cannot silently escape the oracle"
    );
    let mut result = BTreeMap::new();
    for table in &expected {
        let columns: Vec<String> = connection
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap()
            .query_map([], |row| row.get(1))
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        result.insert(format!("{table}_schema"), vec![columns]);
    }
    for table in BATCH_LOGICAL_TABLES {
        let columns: Vec<String> = connection
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap()
            .query_map([], |row| row.get(1))
            .unwrap()
            .map(|row| row.unwrap())
            .filter(|column: &String| {
                !matches!(
                    column.as_str(),
                    "doc_row" | "graph_row" | "link_row" | "diagnostic_row" | "dependency_row"
                )
            })
            .collect();
        let query = format!("SELECT {} FROM {table}", columns.join(","));
        result.insert(format!("{table}_columns"), vec![columns]);
        result.insert(table.into(), batch_sql_rows(connection, &query));
    }
    result.insert("catalog_meta_stable".into(), batch_sql_rows(connection,
        "SELECT schema_version,vault_id,parser_hash,state,vector_cache_lost,vector_loss_unknown,revision_ownership_version,proof_layout_version FROM catalog_meta"));
    result.insert("documents_fts_terms".into(), batch_sql_rows(connection,
        "SELECT term,path,col,offset FROM documents_vocab JOIN documents ON documents_vocab.doc=documents.doc_row"));
    result.insert("graph_fts_terms".into(), batch_sql_rows(connection,
        "SELECT term,target_id,col,offset FROM graph_vocab JOIN graph_rows ON graph_vocab.doc=graph_rows.graph_row"));
    result
}
fn batch_complete_oracle(fixture: &Fixture) {
    let reader = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let before = QueryCatalog::snapshot(&reader).clone();
    let actual = batch_complete_projection(reader.connection());
    for table in [
        "retrieval_units",
        "unit_owners",
        "unit_owner_dependencies",
        "unit_policies",
    ] {
        assert!(
            actual[table].is_empty(),
            "fixture has no registered compact-unit policy: {table}"
        );
    }
    let identity = BuildIdentity {
        selection: CatalogSelection::new(id("vault_projector"), before.generation).unwrap(),
        origin: None,
        vector_cache_lost: false,
        vector_loss_unknown: false,
    };
    let mut builder = NormalizedBuilder::begin(
        &fixture.fs,
        &fixture.writer,
        identity,
        BuildLimits::default(),
    )
    .unwrap();
    let input = scan::scan_input(&fixture.fs, &id("vault_projector")).unwrap();
    let projection =
        scan::project_normalized_with_sink(&fixture.fs, &input, false, &mut builder).unwrap();
    let rebuilt = builder.finish_normalized(&projection).unwrap();
    let sibling = rusqlite::Connection::open_with_flags(
        &rebuilt.path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let expected = batch_complete_projection(&sibling);
    for (table, rows) in &expected {
        assert_eq!(
            &actual[table], rows,
            "complete logical projection differs: {table}"
        );
    }
    assert_eq!(
        QueryCatalog::snapshot(
            &fixture
                .catalog
                .query_snapshot(QueryReadLimits::default())
                .unwrap()
        ),
        &before,
        "oracle builds an unselected sibling without publication"
    );
}
fn batch_public_read_citation(
    fixture: &Fixture,
    source: &RecordId,
    revision: &RecordId,
    bytes: &[u8],
    historical: bool,
) {
    use crate::sources::{CitationScope, CitationState};
    use clap::Parser;
    let payload = format!("sources/{source}/revisions/{revision}/content.md");
    let args = crate::cli::Arguments::try_parse_from([
        "lwiki",
        "--wiki",
        fixture.fs.root().path().to_str().unwrap(),
        "--offline",
        "--json",
        "read",
        "--path",
        payload.as_str(),
        "--max-bytes",
        "8192",
    ])
    .unwrap();
    let (envelope, exit) = crate::cli::execute(&args);
    assert_eq!(exit, 0, "{}", serde_json::to_string(&envelope).unwrap());
    assert_eq!(envelope.data["body"].as_str().unwrap().as_bytes(), bytes);
    let citation: CitationRef =
        serde_json::from_value(envelope.data["source_citation"]["citation"].clone()).unwrap();
    let CitationRef::Source(reference) = &citation else {
        panic!("read must return direct Source citation")
    };
    assert_eq!(&reference.source_id, source);
    assert_eq!(&reference.source_revision, revision);
    assert_eq!(
        reference.span,
        crate::domain::ByteSpan::new(0, bytes.len() as u64).unwrap()
    );
    assert_eq!(reference.quote_hash, Blake3Hash::digest(bytes));
    let store = SourceStore::new(fixture.fs.clone());
    let view = store.view().unwrap();
    let verified = view
        .verify(
            &citation,
            if historical {
                CitationScope::Historical
            } else {
                CitationScope::Current
            },
        )
        .unwrap();
    assert_eq!(verified.quote, bytes);
    assert_eq!(
        verified.state,
        if historical {
            CitationState::Historical
        } else {
            CitationState::Current
        }
    );
    if historical {
        assert!(view.verify(&citation, CitationScope::Current).is_err());
    }
}
#[test]
fn refresh_batch_complete_23_table_oracle_and_public_cited_results() {
    use crate::retrieval::{
        ContextRequest, ContextScope, QueryPlan, SearchFilters, lexical, verification,
    };
    use crate::sources::{CitationScope, CitationState};
    let mut extras = None;
    let fixture = Fixture::with_setup(true, |fixture| {
        batch_fanout_seed(fixture);
        let store = SourceStore::new(fixture.fs.clone());
        let external = store
            .plan_capture(Fixture::request(b"external untouched quote"))
            .unwrap();
        let external_id = external.source_id.clone();
        Fixture::seed(&fixture.fs, external.draft.unwrap());
        let source_path = format!("sources/{external_id}/source.md");
        let parsed = parse_note(&fs::read(fixture.fs.root().path().join(&source_path)).unwrap());
        let bytes = crate::records::edit_note(
            &parsed,
            &BTreeMap::from([("wiki_depends_on_ids".into(), json!(["assertion_opposite"]))]),
            None,
            &parsed.source_hash,
        )
        .unwrap();
        fixture.write(&source_path, &bytes);
        let noop = store
            .plan_capture(Fixture::request(b"noop untouched quote"))
            .unwrap();
        let noop_id = noop.source_id.clone();
        Fixture::seed(&fixture.fs, noop.draft.unwrap());
        extras = Some((external_id, noop_id));
    });
    let (external, noop) = extras.unwrap();
    let other = batch_other_source(&fixture);
    let original_reader = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let original_immutables = batch_old_immutable_bytes(&fixture, &original_reader);
    drop(original_reader);
    let external_before = fs::read(
        fixture
            .fs
            .root()
            .path()
            .join(format!("sources/{external}/source.md")),
    )
    .unwrap();
    let (delta, _) = batch_apply(
        &fixture,
        &[
            (&other, b"beta batchcurrentprobe quote", None),
            (&fixture.source, b"alpha batchcurrentprobe quote", None),
            (&noop, b"noop untouched quote", None),
        ],
    );
    assert!(
        delta
            .records
            .iter()
            .any(|row| row.record.id() == &external && row.eligibility == Eligibility::Stale)
    );
    batch_complete_oracle(&fixture);
    let reader = fixture
        .catalog
        .query_snapshot(QueryReadLimits::default())
        .unwrap();
    let plan = QueryPlan {
        filters: SearchFilters {
            source_ids: vec![fixture.source.clone(), other.clone()],
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(
        lexical::search_catalog(&reader, "first", &plan)
            .unwrap()
            .hits
            .is_empty(),
        "superseded captured text is absent from current discovery"
    );
    let current: BTreeMap<_, _> = [&fixture.source, &other]
        .into_iter()
        .map(|source| {
            (
                source.clone(),
                record_id(
                    &reader.record(source).unwrap().unwrap().record,
                    "wiki_current_revision",
                )
                .unwrap(),
            )
        })
        .collect();
    drop(reader);
    let context = verification::context(
        &fixture.catalog,
        None,
        "batchcurrentprobe",
        &ContextRequest {
            scope: ContextScope::IndexedEvidence,
            documents: plan,
            ..Default::default()
        },
    )
    .unwrap();
    let store = SourceStore::new(fixture.fs.clone());
    let view = store.view().unwrap();
    let mut seen = BTreeSet::new();
    for passage in context.passages() {
        for citation in &passage.citations {
            let CitationRef::Source(reference) = citation else {
                panic!("current context needs direct Source citation")
            };
            assert_eq!(
                current.get(&reference.source_id),
                Some(&reference.source_revision)
            );
            let quote = if reference.source_id == fixture.source {
                b"alpha batchcurrentprobe quote".as_slice()
            } else {
                b"beta batchcurrentprobe quote".as_slice()
            };
            assert_eq!(reference.quote_hash, Blake3Hash::digest(quote));
            assert_eq!(
                reference.span,
                crate::domain::ByteSpan::new(0, quote.len() as u64).unwrap()
            );
            let verified = view.verify(citation, CitationScope::Current).unwrap();
            assert_eq!(verified.state, CitationState::Current);
            assert_eq!(verified.quote, quote);
            assert_eq!(passage.text.as_bytes(), quote);
            seen.insert(reference.source_id.clone());
        }
    }
    assert_eq!(seen, current.keys().cloned().collect());
    drop(view);
    for (source, revision) in &current {
        batch_public_read_citation(
            &fixture,
            source,
            revision,
            if source == &fixture.source {
                b"alpha batchcurrentprobe quote"
            } else {
                b"beta batchcurrentprobe quote"
            },
            false,
        );
    }
    batch_public_read_citation(
        &fixture,
        &fixture.source,
        &fixture.first,
        b"first capture quote",
        true,
    );
    // Normalized catalogs expose historical discovery through a lexical
    // document Snapshot, not the unsupported strict Historical coordinator.
    // Exact citation authority is checked separately below with SourceView.
    let historical = verification::context(
        &fixture.catalog,
        None,
        "first",
        &ContextRequest {
            scope: ContextScope::Snapshot,
            documents: QueryPlan {
                filters: SearchFilters {
                    source_ids: vec![fixture.source.clone()],
                    include_historical: true,
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        historical
            .warnings()
            .iter()
            .any(|warning| warning == "unverified index snapshot; citations suppressed"),
        "Snapshot output must explicitly disclose that it is unverified"
    );
    let historical_path = path(&format!(
        "sources/{}/revisions/{}/content.md",
        fixture.source, fixture.first
    ));
    let historical_store = SourceStore::new(fixture.fs.clone());
    let historical_view = historical_store.view().unwrap();
    let mut historical_passages = 0;
    for passage in historical.passages() {
        assert!(
            passage.citations.is_empty(),
            "Snapshot must suppress direct citations"
        );
        if passage.locator.path == historical_path {
            let owner = passage
                .locator
                .record
                .as_ref()
                .expect("captured Snapshot passage retains its Revision owner");
            assert_eq!(owner.vault_id, id("vault_projector"));
            assert_eq!(owner.record_id, fixture.first);
            assert_eq!(owner.expected_kind, RecordKind::Revision);
            assert_eq!(passage.eligibility, Eligibility::Historical);
            assert_eq!(passage.text.as_bytes(), b"first capture quote");
            assert_eq!(
                passage.locator.observed_hash,
                Blake3Hash::digest(b"first capture quote")
            );
            assert_eq!(
                passage.span,
                crate::domain::ByteSpan::new(0, b"first capture quote".len() as u64).unwrap()
            );
            // This separate exact check supplies citation authority; the cached
            // Snapshot itself deliberately supplies no verified citation.
            let citation = CitationRef::Source(crate::domain::SourceSpanRef {
                source_id: fixture.source.clone(),
                source_revision: fixture.first.clone(),
                span: passage.span,
                quote_hash: Blake3Hash::digest(b"first capture quote"),
            });
            let verified = historical_view
                .verify(&citation, CitationScope::Historical)
                .unwrap();
            assert_eq!(verified.state, CitationState::Historical);
            assert_eq!(verified.quote, b"first capture quote");
            assert!(
                historical_view
                    .verify(&citation, CitationScope::Current)
                    .is_err()
            );
            historical_passages += 1;
        }
    }
    assert!(
        historical_passages > 0,
        "historical Snapshot discovery must return the exact old capture with its Revision owner"
    );
    for (path, bytes) in &original_immutables {
        assert_eq!(
            fs::read(fixture.fs.root().path().join(path.as_str())).unwrap(),
            *bytes,
            "fresh batch preserves exact old immutable bytes: {path}"
        );
    }
    drop(historical_view);
    batch_apply(
        &fixture,
        &[
            (&fixture.source, b"first capture quote", None),
            (&other, b"other capture quote", None),
            (&noop, b"noop untouched quote", None),
        ],
    );
    batch_complete_oracle(&fixture);
    for (path, bytes) in &original_immutables {
        assert_eq!(
            fs::read(fixture.fs.root().path().join(path.as_str())).unwrap(),
            *bytes,
            "historical restore preserves exact old immutable bytes: {path}"
        );
    }
    batch_public_read_citation(
        &fixture,
        &fixture.source,
        &fixture.first,
        b"first capture quote",
        false,
    );
    batch_public_read_citation(
        &fixture,
        &fixture.source,
        &current[&fixture.source],
        b"alpha batchcurrentprobe quote",
        true,
    );
    assert_eq!(
        fs::read(
            fixture
                .fs
                .root()
                .path()
                .join(format!("sources/{external}/source.md"))
        )
        .unwrap(),
        external_before
    );
}

#[test]
fn refresh_batch_rechecksummed_member_allocation_revision_owner_matrix_refuses() {
    use crate::changes::indexed_refresh::IndexedWriteOperation;
    for tamper in [
        "missing_member",
        "shared_revision",
        "allocation_key",
        "allocation_gap",
        "missing_owner",
        "extra_owner",
    ] {
        let fixture = Fixture::new(true);
        let other = batch_other_source(&fixture);
        let (reader, plans) = batch_plans(
            &fixture,
            &[
                (&fixture.source, b"first capture quote", None),
                (&other, b"fresh matrix member", None),
            ],
        );
        let projected = project_refresh_batch(
            &fixture.fs,
            &reader,
            plans,
            &RefreshProjectionLimits::default(),
        )
        .unwrap()
        .unwrap();
        let session =
            IndexedRefreshSession::prepare_write(&fixture.catalog, &fixture.writer, projected)
                .unwrap();
        let mut proof = session.proof().clone();
        drop(session);
        drop(reader);
        let before = batch_member_source_bytes(&fixture, &proof);
        fixture.write("unknown-matrix.bin", b"unknown unrelated fixture bytes");
        let visible = batch_visible_inventory(&fixture);
        let allocated = fs::read_dir(fixture.fs.root().path().join("changes"))
            .unwrap()
            .count();
        let old = batch_old_immutable_bytes(
            &fixture,
            &fixture
                .catalog
                .query_snapshot(QueryReadLimits::default())
                .unwrap(),
        );
        let mut rows = batch_retained_rows(&fixture, &proof);
        let base = proof.base.clone();
        let engine = ChangeEngine::new(fixture.fs.clone()).unwrap();
        match tamper {
            "missing_member" => {
                let IndexedWriteOperation::SourceRefreshBatch { refreshes, .. } =
                    proof.operation.as_mut().unwrap()
                else {
                    unreachable!()
                };
                refreshes.retain(|target| !target.no_op);
            }
            "shared_revision" => {
                let IndexedWriteOperation::SourceRefreshBatch { refreshes, .. } =
                    proof.operation.as_mut().unwrap()
                else {
                    unreachable!()
                };
                let noop = refreshes
                    .iter()
                    .find(|target| target.no_op)
                    .unwrap()
                    .revision_id
                    .clone();
                refreshes
                    .iter_mut()
                    .find(|target| !target.no_op)
                    .unwrap()
                    .revision_id = noop;
            }
            "allocation_key" | "allocation_gap" => {
                let (mut manifest, old_hash) =
                    engine.load_manifest(&proof.change.change_id).unwrap();
                let mut journal =
                    crate::changes::journal::load_journal(&fixture.fs, &manifest, &old_hash)
                        .unwrap();
                let (key, revision) = manifest.allocated_ids.pop_first().unwrap();
                let index: usize = key.strip_prefix("revision_").unwrap().parse().unwrap();
                manifest.allocated_ids.insert(
                    if tamper == "allocation_gap" {
                        format!("revision_{}", index + 1)
                    } else {
                        "revision".into()
                    },
                    revision,
                );
                let hash = Blake3Hash::digest(serde_json::to_vec(&manifest).unwrap());
                proof.change.manifest_hash = hash.clone();
                fixture.write(
                    crate::changes::prepare::manifest_path(&proof.change.change_id)
                        .unwrap()
                        .as_str(),
                    &crate::changes::prepare::render_prepared_note(&manifest, &hash).unwrap(),
                );
                let mut bytes = Vec::new();
                for frame in &mut journal.frames {
                    frame.manifest_hash = hash.clone();
                    bytes.extend(crate::changes::journal::encode_frame(frame).unwrap());
                }
                fixture.write(
                    crate::changes::journal::journal_path(&proof.change.change_id)
                        .unwrap()
                        .as_str(),
                    &bytes,
                );
                for owner in &mut rows.owners {
                    owner.change = proof.change.clone();
                }
                assert_eq!(
                    engine.load_manifest(&proof.change.change_id).unwrap(),
                    (manifest, hash)
                );
            }
            "missing_owner" => {
                rows.owners.clear();
            }
            "extra_owner" => {
                let mut owner = rows.owners[0].clone();
                owner.key.revision_component = "revision_unallocated_extra".into();
                rows.owners.push(owner);
            }
            _ => unreachable!(),
        }
        let forged = batch_rewrite_retained_rows(&fixture, proof, rows);
        let (fixture, result) = batch_public_apply(fixture, &forged.change);
        let error = result.err().unwrap();
        assert!(
            matches!(
                error.code,
                ErrorCode::RecoveryRequired | ErrorCode::IndexCorrupt | ErrorCode::RecordInvalid
            ),
            "{tamper}: {error:?}"
        );
        batch_assert_source_bytes(&fixture, &before);
        assert_eq!(
            batch_visible_inventory(&fixture),
            visible,
            "{tamper}: all visible canonical/unrelated bytes unchanged"
        );
        assert_eq!(
            fs::read_dir(fixture.fs.root().path().join("changes"))
                .unwrap()
                .count(),
            allocated
        );
        for (path, bytes) in old {
            assert_eq!(
                fs::read(fixture.fs.root().path().join(path.as_str())).unwrap(),
                bytes
            );
        }
        assert_eq!(
            QueryCatalog::snapshot(
                &fixture
                    .catalog
                    .query_snapshot(QueryReadLimits::default())
                    .unwrap()
            ),
            &base
        );
        assert!(
            crate::changes::operation_authority::load(
                &fixture.fs,
                &id("vault_projector"),
                crate::changes::operation_authority::Presence::Required
            )
            .unwrap()
            .unwrap()
            .active()
            .is_none()
        );
    }
}
#[test]
fn refresh_batch_descriptor_strict_roundtrip_and_malformed_flags_refuse() {
    use crate::changes::indexed_refresh::{IndexedRefreshSourceDependency, IndexedWriteOperation};
    let fixture = Fixture::new(true);
    let other = batch_other_source(&fixture);
    let (reader, plans) = batch_plans(
        &fixture,
        &[
            (&fixture.source, b"first capture quote", None),
            (&other, b"descriptor fresh member", None),
        ],
    );
    let parts = project_refresh_batch(
        &fixture.fs,
        &reader,
        plans,
        &RefreshProjectionLimits::default(),
    )
    .unwrap()
    .unwrap()
    .into_parts();
    for dependencies in [false, true] {
        let mut operation = parts.operation.clone();
        if dependencies {
            let IndexedWriteOperation::SourceRefreshBatch {
                dependent_sources, ..
            } = &mut operation
            else {
                unreachable!()
            };
            dependent_sources.push(IndexedRefreshSourceDependency {
                source_id: id("source_external_descriptor"),
                source_bytes: "exact witness bytes".into(),
            });
        }
        operation.validate().unwrap();
        let value = serde_json::to_value(&operation).unwrap();
        assert_eq!(
            serde_json::from_value::<IndexedWriteOperation>(value.clone()).unwrap(),
            operation
        );
        for location in ["operation", "member", "dependency"] {
            if location == "dependency" && !dependencies {
                continue;
            }
            let mut forged = value.clone();
            match location {
                "operation" => forged["unknown"] = json!(true),
                "member" => forged["refreshes"][0]["unknown"] = json!(true),
                "dependency" => forged["dependent_sources"][0]["unknown"] = json!(true),
                _ => unreachable!(),
            }
            assert!(serde_json::from_value::<IndexedWriteOperation>(forged).is_err());
        }
        for flag in ["no_op", "reused"] {
            let mut forged = value.clone();
            let noop = forged["refreshes"]
                .as_array()
                .unwrap()
                .iter()
                .position(|target| target["no_op"] == true)
                .unwrap();
            forged["refreshes"][noop][flag] = json!(false);
            assert!(
                serde_json::from_value::<IndexedWriteOperation>(forged)
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }
    }
}

#[test]
fn refresh_batch_escaped_witness_encoded_preadmission_nearest_boundary() {
    // Defensive lowered-cap admission coverage of the production counted path;
    // this does not qualify valid-vault capacity at the 64/256 MiB ceilings.
    for boundary in ["proof_refusal", "delta_refusal", "nearest_admitted"] {
        let fixture = Fixture::with_setup(true, |fixture| {
            let source = format!("sources/{}/source.md", fixture.source);
            let parsed = parse_note(&fs::read(fixture.fs.root().path().join(&source)).unwrap());
            let bytes = crate::records::edit_note(
                &parsed,
                &BTreeMap::from([("title".into(), json!("escape\\\"\t".repeat(200)))]),
                None,
                &parsed.source_hash,
            )
            .unwrap();
            fixture.write(&source, &bytes);
        });
        let other = batch_other_source(&fixture);
        let (reader, plans) = batch_plans(
            &fixture,
            &[
                (&fixture.source, b"first capture quote", None),
                (&other, b"encoded fresh member", None),
            ],
        );
        let parts = project_refresh_batch(
            &fixture.fs,
            &reader,
            plans,
            &RefreshProjectionLimits::default(),
        )
        .unwrap()
        .unwrap()
        .into_parts();
        let proof_encoded =
            serde_json::to_vec(&(&parts.operation, &parts.before, &parts.after, &parts.base))
                .unwrap()
                .len();
        let delta_encoded = serde_json::to_vec(&(
            &parts.operation,
            &parts.before,
            &parts.after,
            &parts.base,
            &parts.delta,
        ))
        .unwrap()
        .len();
        let witness = batch_recovery_targets_from_operation(&parts.operation)
            .iter()
            .find(|target| target.no_op)
            .unwrap()
            .unchanged_source
            .as_ref()
            .unwrap();
        assert!(
            serde_json::to_vec(witness).unwrap().len() > witness.len(),
            "escaped witness expands in retained encoding"
        );
        let proof_maximum = proof_encoded + 16 * 1024 - usize::from(boundary == "proof_refusal");
        let delta_maximum = delta_encoded + 1024 * 1024 - usize::from(boundary == "delta_refusal");
        let sources: BTreeMap<_, _> = [&fixture.source, &other]
            .into_iter()
            .map(|source| {
                let name = path(&format!("sources/{source}/source.md"));
                (
                    name.clone(),
                    fs::read(fixture.fs.root().path().join(name.as_str())).unwrap(),
                )
            })
            .collect();
        let authority_path = fixture.fs.root().path().join(".wiki/state/operations.json");
        let authority = fs::read(&authority_path).unwrap();
        let base = parts.base.clone();
        drop(reader);
        let result = IndexedRefreshSession::prepare_batch_with_encoded_limits(
            &fixture.catalog,
            &fixture.writer,
            parts,
            proof_maximum,
            delta_maximum,
        );
        if boundary == "nearest_admitted" {
            let session = result.unwrap();
            assert_eq!(&session.proof().base, &base);
        } else {
            let error = result.err().unwrap();
            assert_eq!(
                error.code,
                ErrorCode::BudgetExceeded,
                "{boundary}: {error:?}"
            );
            assert!(
                !fixture.fs.root().path().join("changes").exists(),
                "encoded refusal precedes retained Change allocation"
            );
            assert_eq!(fs::read(&authority_path).unwrap(), authority);
            assert_eq!(
                QueryCatalog::snapshot(
                    &fixture
                        .catalog
                        .query_snapshot(QueryReadLimits::default())
                        .unwrap()
                ),
                &base
            );
        }
        batch_assert_source_bytes(&fixture, &sources);
    }
}
fn batch_recovery_targets_from_operation(
    operation: &crate::changes::indexed_refresh::IndexedWriteOperation,
) -> &[crate::changes::indexed_refresh::IndexedRefreshTarget] {
    let crate::changes::indexed_refresh::IndexedWriteOperation::SourceRefreshBatch {
        refreshes,
        ..
    } = operation
    else {
        panic!("batch required")
    };
    refreshes
}
#[test]
fn refresh_batch_shared_planning_reader_deadline_expires_for_noop_and_changed() {
    for changed in [false, true] {
        let fixture = Fixture::new(true);
        let reader = fixture
            .catalog
            .query_snapshot(QueryReadLimits {
                max_elapsed_ms: 100,
                ..Default::default()
            })
            .unwrap();
        let plan = SourceStore::new(fixture.fs.clone())
            .plan_refresh_indexed(
                &reader,
                &fixture.source,
                Fixture::request(if changed {
                    b"deadline fresh"
                } else {
                    b"first capture quote"
                }),
                None,
                &SourceRefreshLimits::default(),
            )
            .unwrap();
        let before = fs::read(
            fixture
                .fs
                .root()
                .path()
                .join(format!("sources/{}/source.md", fixture.source)),
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(110));
        let error = project_refresh_batch(
            &fixture.fs,
            &reader,
            vec![plan],
            &RefreshProjectionLimits::default(),
        )
        .err()
        .unwrap();
        assert_eq!(
            error.code,
            ErrorCode::BudgetExceeded,
            "elapsed nonzero shared reader budget: {error:?}"
        );
        assert_eq!(
            fs::read(
                fixture
                    .fs
                    .root()
                    .path()
                    .join(format!("sources/{}/source.md", fixture.source))
            )
            .unwrap(),
            before
        );
        assert!(!fixture.fs.root().path().join("changes").exists());
    }
}
#[test]
fn refresh_batch_default_4096_joint_query_rows_refuse_individually_admitted_members() {
    let fixture = Fixture::with_setup(true, |fixture| {
        let other = batch_other_source(fixture);
        let parsed =
            parse_note(&fs::read(fixture.fs.root().path().join("evidence_other.md")).unwrap());
        let other_revision =
            record_id(parsed.canonical.as_ref().unwrap(), "wiki_source_revision").unwrap();
        for (member, revision) in [(0, &fixture.first), (1, &other_revision)] {
            for index in 0..400 {
                let name = format!("scope/member-{member}-{index}.md");
                fixture.write(
                    &name,
                    &note(
                        "page",
                        &format!("page_scope_{member}_{index}"),
                        json!({"wiki_status":"reviewed", "wiki_depends_on_ids":[revision]}),
                        b"bounded joint dependency",
                    ),
                );
            }
        }
        assert_ne!(fixture.source, other);
    });
    let other = batch_other_source(&fixture);
    let mut scalar_rows = Vec::new();
    for source in [&fixture.source, &other] {
        let reader = fixture
            .catalog
            .query_snapshot(QueryReadLimits::default())
            .unwrap();
        let plan = SourceStore::new(fixture.fs.clone())
            .plan_refresh_indexed(
                &reader,
                source,
                Fixture::request(b"row budget fresh member"),
                None,
                &SourceRefreshLimits::default(),
            )
            .unwrap();
        assert!(
            project_refresh(
                &fixture.fs,
                &reader,
                plan,
                &RefreshProjectionLimits::default()
            )
            .unwrap()
            .is_some()
        );
        scalar_rows.push(reader.usage().rows);
    }
    let (reader, plans) = batch_plans(
        &fixture,
        &[
            (&fixture.source, b"row budget fresh member", None),
            (&other, b"row budget fresh member", None),
        ],
    );
    let base = QueryCatalog::snapshot(&reader).clone();
    let old = batch_old_immutable_bytes(
        &fixture,
        &fixture
            .catalog
            .query_snapshot(QueryReadLimits::default())
            .unwrap(),
    );
    let sources: BTreeMap<_, _> = [&fixture.source, &other]
        .into_iter()
        .map(|source| {
            let name = path(&format!("sources/{source}/source.md"));
            (
                name.clone(),
                fs::read(fixture.fs.root().path().join(name.as_str())).unwrap(),
            )
        })
        .collect();
    let error = project_refresh_batch(
        &fixture.fs,
        &reader,
        plans,
        &RefreshProjectionLimits::default(),
    )
    .err()
    .unwrap();
    eprintln!(
        "default shared row ceiling=4096; dependers=800; individual_rows={scalar_rows:?}; joint_rows={}; refusal={error:?}",
        reader.usage().rows
    );
    assert_eq!(error.code, ErrorCode::BudgetExceeded);
    assert!(scalar_rows.iter().all(|rows| *rows <= 4096));
    assert_eq!(
        reader.usage().rows,
        4096,
        "actual first exhausted pipeline boundary is the default shared selected-row budget"
    );
    assert!(reader.usage().rows > *scalar_rows.iter().max().unwrap());
    assert!(!fixture.fs.root().path().join("changes").exists());
    batch_assert_source_bytes(&fixture, &sources);
    for (path, bytes) in old {
        assert_eq!(
            fs::read(fixture.fs.root().path().join(path.as_str())).unwrap(),
            bytes
        );
    }
    drop(reader);
    assert_eq!(
        QueryCatalog::snapshot(
            &fixture
                .catalog
                .query_snapshot(QueryReadLimits::default())
                .unwrap()
        ),
        &base
    );
}

fn batch_visible_inventory(fixture: &Fixture) -> BTreeMap<std::path::PathBuf, Vec<u8>> {
    let root = fixture.fs.root().path();
    let mut queue = vec![root.to_path_buf()];
    let mut result = BTreeMap::new();
    while let Some(directory) = queue.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path == root.join(".wiki") || path == root.join("changes") {
                continue;
            }
            if entry.file_type().unwrap().is_dir() {
                queue.push(path);
            } else {
                result.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    result
}

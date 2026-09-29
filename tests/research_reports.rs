//! Research report publication and page proposals use actual local ledgers/changes.
use lwiki as library;
#[path = "fixtures/p18/common.rs"]
mod common;
#[allow(dead_code)]
#[path = "fixtures/p16c/common.rs"]
mod provider;
use common::{Fixture, Mock, id};
use lwiki::{
    catalog::{Catalog, CatalogGraphValidator},
    changes::{ChangeEngine, ChangeStatus, PreparedChange},
    domain::*,
    graph::packet::canonical_json,
    jobs::*,
    providers::types::DispatchPurpose,
    research::{
        ResearchLimits, ResearchPassage, ResearchScope, plan, report, stages,
        synthesis::{self, ClaimStatus, ValidatedSynthesis},
    },
    sources::*,
    vault::WriterPermit,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Duration};

struct Reports {
    f: Fixture,
    scope: ResearchScope,
    job: JobLedger,
    passage: ResearchPassage,
    task: TaskSpec,
    records: Vec<RecordRef>,
}
impl Reports {
    fn new(apply: bool, update: bool) -> Self {
        Self::with_pages(apply, if update { 1 } else { 0 })
    }
    fn with_pages(apply: bool, page_count: usize) -> Self {
        let f = Fixture::new();
        let scope = ResearchScope {
            version: 1,
            question: "What does this source establish?".into(),
            exclusions: vec![],
            explicit_urls: vec![],
            generation_profile: "primary".into(),
            search_profile: None,
            limits: ResearchLimits::default(),
            apply,
        };
        let citation = CitationRef::Source(SourceSpanRef {
            source_id: f.packet.source_id.clone(),
            source_revision: f.packet.source_revision.clone(),
            span: ByteSpan::new(0, 24).unwrap(),
            quote_hash: Blake3Hash::digest(b"Ada works for Acme.\nAda "),
        });
        let view = SourceView::from_fs_bounded(&f.fs, 64 * 1024 * 1024, 4096).unwrap();
        let verified = view.verify(&citation, CitationScope::Current).unwrap();
        let passage = ResearchPassage {
            citation,
            quote: String::from_utf8(verified.quote).unwrap(),
            dependencies: verified.dependencies,
        };
        let mut records = Vec::new();
        let mut page_reads = Vec::new();
        if page_count > 0 {
            std::fs::create_dir_all(f.temp.path().join("pages")).unwrap();
        }
        for index in 0..page_count {
            let suffix = if index == 0 {
                String::new()
            } else {
                (index + 1).to_string()
            };
            let page_id = id(&format!("page_known{suffix}"));
            let path = VaultRelativePath::new(format!("pages/known{suffix}.md")).unwrap();
            let bytes = format!("---\nwiki_schema: \"1\"\nwiki_id: {page_id}\nwiki_kind: page\nwiki_status: draft\ntitle: Existing heading\n---\nOriginal user page.\n").into_bytes();
            std::fs::write(f.fs.root().resolve(&path).unwrap(), &bytes).unwrap();
            page_reads.push(lwiki::changes::ReadDependency {
                path,
                expected: lwiki::vault::ExpectedState::Hash(Blake3Hash::digest(bytes)),
            });
            records.push(RecordRef {
                vault_id: id("vault_test"),
                record_id: page_id,
                expected_kind: RecordKind::Page,
            });
        }
        let run = id("run_reports");
        let mut descriptor = plan::generation_task(
            &run,
            &scope,
            TaskStage::Synthesize,
            1,
            &[],
            std::slice::from_ref(&passage),
            &records,
            &f.service,
            0,
            vec![],
        )
        .unwrap();
        descriptor = plan::with_read_preconditions(descriptor, &page_reads).unwrap();
        let frozen_reads = descriptor.task.source_bindings.clone();
        let scope_bytes = canonical_json(&scope).unwrap();
        let scope_hash = Blake3Hash::digest(&scope_bytes);
        let scope_path = VaultRelativePath::new(format!("runs/{run}/inputs/scope.json")).unwrap();
        std::fs::create_dir_all(f.temp.path().join("runs/run_reports/inputs")).unwrap();
        std::fs::write(f.fs.root().resolve(&scope_path).unwrap(), &scope_bytes).unwrap();
        std::fs::write(
            f.fs.root().resolve(&descriptor.task.input.path).unwrap(),
            &descriptor.bytes,
        )
        .unwrap();
        let summary = f.service.summary();
        let mut spec = RunSpec {
            version: 1,
            run_id: run.clone(),
            vault_id: id("vault_test"),
            title: "Report tests".into(),
            created_at_utc_ms: f.request.created_at_utc_ms,
            deadline_utc_ms: f.request.deadline_utc_ms,
            scope: RunScope {
                research: Some(ResearchGenesisV1 {
                    version: 1,
                    scope: BoundedPayloadRef {
                        path: scope_path,
                        hash: scope_hash.clone(),
                        byte_len: scope_bytes.len() as u64,
                    },
                    limits: ResearchAdmissionLimits {
                        rounds: scope.limits.rounds,
                        sources: scope.limits.sources,
                    },
                    initial_binding: BindingEpochV1 {
                        version: 1,
                        number: 0,
                        config_fingerprint: summary.config_fingerprint.clone(),
                        source_snapshot: None,
                        input_records: vec![],
                        read_preconditions: frozen_reads.clone(),
                        services: vec![ServiceBindingV1 {
                            profile_id: summary.profile_id.clone(),
                            capability: Capability::Generate,
                            profile_fingerprint: summary.profile_fingerprint.clone(),
                            endpoint_fingerprint: summary.endpoint_fingerprint.clone(),
                        }],
                    },
                }),
                operation: "research".into(),
                question: Some(scope.question.clone()),
                exclusions: vec![],
                source_snapshot: None,
                input_records: vec![],
                read_preconditions: frozen_reads.clone(),
                profile_fingerprints: BTreeMap::from([(
                    summary.profile_id,
                    summary.profile_fingerprint,
                )]),
                scope_payload_hash: Some(scope_hash),
            },
            config_fingerprint: summary.config_fingerprint,
            input_fingerprint: Blake3Hash::digest([]),
            limits: LifetimeLimits::default(),
            tasks: vec![descriptor.task.clone()],
            prior_accounting: PriorAccounting::None,
        };
        spec.input_fingerprint = lwiki::jobs::tasks::input_fingerprint(&spec).unwrap();
        let job = JobLedger::new(f.fs.clone(), id("vault_test"), run, f.options.clone()).unwrap();
        let writer = WriterPermit::acquire(f.fs.root(), Duration::from_secs(2)).unwrap();
        job.create(&writer, spec).unwrap();
        drop(writer);
        job.start().unwrap();
        Self {
            f,
            scope,
            job,
            passage,
            task: descriptor.task,
            records,
        }
    }
    fn value(&self, update: bool) -> Value {
        let proposal = if update {
            json!({"kind":"update_page","record":self.records[0],"body":"Model proposal body.","citations":[self.passage.citation]})
        } else {
            json!({"kind":"create_page","title":"A generated page","body":"Model proposal body.","citations":[self.passage.citation]})
        };
        json!({"sections":[{"heading":"Findings","claims":[{"text":"A deliberately unsupported conclusion.","citations":[self.passage.citation]},{"text":"No supplied evidence for this claim.","citations":[]}]}],"unanswered_questions":["An unanswered question."],"proposed_changes":[proposal]})
    }
    fn paid(&self, value: Value) -> ValidatedSynthesis {
        let view = SourceView::from_fs_bounded(&self.f.fs, 64 * 1024 * 1024, 4096).unwrap();
        let validated = synthesis::validate(
            &canonical_json(&value).unwrap(),
            &self.scope.limits.stage,
            std::slice::from_ref(&self.passage.citation),
            &self.records,
            &view,
        )
        .unwrap();
        let d = self.f.dispatcher(Mock::response(value));
        let outcome = d
            .execute(
                &self.job,
                &self.f.service,
                &self.task.key,
                DispatchPurpose::Task,
            )
            .unwrap_or_else(|e| panic!("dispatch failed: {:?}", e.error));
        stages::publish_generation(
            &self.f.fs,
            &self.job,
            &self.task,
            outcome,
            self.f.request.created_at_utc_ms,
        )
        .unwrap();
        validated
    }
    fn build(&self, validated: Option<ValidatedSynthesis>) -> lwiki::research::ResearchReport {
        report::build(
            &self.f.fs,
            &self.job,
            &self.scope,
            std::slice::from_ref(&self.passage),
            &[],
            validated,
            vec![],
            "coverage",
            false,
        )
        .unwrap()
    }
}
fn after_bytes(f: &Fixture, change: &PreparedChange) -> Vec<u8> {
    let inspected = ChangeEngine::new(f.fs.clone())
        .unwrap()
        .inspect(&change.change_id)
        .unwrap();
    let path = &inspected.manifest.operations[0]
        .after_payload
        .as_ref()
        .unwrap()
        .path;
    std::fs::read(f.fs.root().resolve(path).unwrap()).unwrap()
}
#[test]
fn page_proposals_are_unassessed_staged_idempotently_and_survive_cache_loss() {
    let r = Reports::new(false, false);
    let validated = r.paid(r.value(false));
    let first = r.build(Some(validated.clone()));
    assert!(
        first
            .claim_assessments
            .iter()
            .all(|a| a.status == ClaimStatus::Unassessed)
    );
    assert!(first.claim_assessments[0].provenance_verified);
    assert!(!first.claim_assessments[1].provenance_verified);
    assert!(first.gaps.iter().any(|g| g.code == "unassessed_claim"));
    assert_eq!(first.proposed_changes.len(), 1);
    let engine = ChangeEngine::new(r.f.fs.clone()).unwrap();
    let prepared = engine
        .inspect(&first.proposed_changes[0].change_id)
        .unwrap();
    assert_eq!(prepared.status, ChangeStatus::Prepared);
    assert!(
        !r.f.fs
            .root()
            .resolve(&prepared.manifest.operations[0].target)
            .unwrap()
            .exists()
    );
    let bytes = after_bytes(&r.f, &first.proposed_changes[0]);
    let note = lwiki::records::parse_note(&bytes);
    assert!(
        note.canonical
            .unwrap()
            .title()
            .starts_with("Unassessed research:")
    );
    assert!(
        String::from_utf8(after_bytes(&r.f, &first.proposed_changes[0]))
            .unwrap()
            .contains("claims require review")
    );
    assert!(
        r.passage
            .dependencies
            .iter()
            .all(|dep| prepared.manifest.read_preconditions.contains(dep))
    );
    let output = report::publish(&r.f.fs, &r.job, &first).unwrap();
    let second = r.build(Some(validated));
    assert_eq!(
        canonical_json(&first).unwrap(),
        canonical_json(&second).unwrap()
    );
    assert_eq!(report::publish(&r.f.fs, &r.job, &second).unwrap(), output);
    let cache = r.f.temp.path().join(".wiki/cache");
    if cache.exists() {
        std::fs::remove_dir_all(cache).unwrap();
    }
    let latest = report::latest(&r.f.fs, &r.job.inspect().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(latest.proposed_changes, first.proposed_changes);
    assert_eq!(r.job.inspect().unwrap().budget.dispatched_requests, 1);
}
#[test]
fn explicit_apply_creates_page_once_without_accepting_assertions() {
    let r = Reports::new(true, false);
    let validated = r.paid(r.value(false));
    let first = r.build(Some(validated.clone()));
    let engine = ChangeEngine::new(r.f.fs.clone()).unwrap();
    let change = engine
        .inspect(&first.proposed_changes[0].change_id)
        .unwrap();
    assert_eq!(change.status, ChangeStatus::Committed);
    let path =
        r.f.fs
            .root()
            .resolve(&change.manifest.operations[0].target)
            .unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert!(
        String::from_utf8(bytes.clone())
            .unwrap()
            .contains("Unassessed model-generated proposal")
    );
    let second = r.build(Some(validated));
    assert_eq!(first.proposed_changes, second.proposed_changes);
    assert_eq!(
        canonical_json(&first).unwrap(),
        canonical_json(&second).unwrap()
    );
    assert_eq!(std::fs::read(path).unwrap(), bytes);
    assert_eq!(r.job.inspect().unwrap().budget.dispatched_requests, 1);
}
#[test]
fn changed_page_keeps_original_expected_hash_and_refuses_apply() {
    let r = Reports::new(false, true);
    let validated = r.paid(r.value(true));
    let first = r.build(Some(validated.clone()));
    let engine = ChangeEngine::new(r.f.fs.clone()).unwrap();
    let change = engine
        .inspect(&first.proposed_changes[0].change_id)
        .unwrap();
    let target =
        r.f.fs
            .root()
            .resolve(&change.manifest.operations[0].target)
            .unwrap();
    let original = std::fs::read(&target).unwrap();
    assert_eq!(
        change.manifest.operations[0].before,
        lwiki::vault::ExpectedState::Hash(Blake3Hash::digest(&original))
    );
    let modified = String::from_utf8(original)
        .unwrap()
        .replace("Original user page.", "New user edit.")
        .into_bytes();
    std::fs::write(&target, &modified).unwrap();
    let writer = WriterPermit::acquire(r.f.fs.root(), Duration::from_secs(2)).unwrap();
    let catalog = Catalog::new(r.f.fs.clone(), id("vault_test"));
    assert!(
        engine
            .apply(
                &writer,
                &first.proposed_changes[0],
                &CatalogGraphValidator,
                &catalog
            )
            .is_err()
    );
    drop(writer);
    assert_eq!(std::fs::read(&target).unwrap(), modified);
    let second = r.build(Some(validated));
    assert_eq!(first.proposed_changes, second.proposed_changes);
    assert!(second.warnings.iter().any(|w| w.contains("changed target")));
    assert_eq!(std::fs::read(target).unwrap(), modified);
}
#[test]
fn edit_after_paid_synthesis_before_first_apply_build_is_preserved() {
    let r = Reports::new(true, true);
    let validated = r.paid(r.value(true));
    let path = r.f.temp.path().join("pages/known.md");
    let original = std::fs::read(&path).unwrap();
    let changed = String::from_utf8(original.clone())
        .unwrap()
        .replace("Original user page.", "User edit after paid synthesis.")
        .into_bytes();
    std::fs::write(&path, &changed).unwrap();
    let change_paths = || {
        r.f.fs
            .root()
            .scan_markdown()
            .unwrap()
            .into_iter()
            .filter(|path| path.as_str().starts_with("changes/"))
            .collect::<Vec<_>>()
    };
    let changes_before = change_paths();
    let first = r.build(Some(validated.clone()));
    assert!(first.partial);
    assert!(first.gaps.iter().any(|g| g.code == "proposal_conflict"));
    assert!(first.proposed_changes.is_empty());
    assert_eq!(std::fs::read(&path).unwrap(), changed);
    assert_eq!(change_paths(), changes_before);
    // Immutable task authority still binds the page bytes seen before generation.
    assert!(
        r.task
            .source_bindings
            .iter()
            .any(|read| read.path.as_str() == "pages/known.md"
                && read.expected
                    == lwiki::vault::ExpectedState::Hash(Blake3Hash::digest(&original)))
    );
    let second = r.build(Some(validated));
    assert_eq!(
        canonical_json(&first).unwrap(),
        canonical_json(&second).unwrap()
    );
    let published = report::publish(&r.f.fs, &r.job, &first).unwrap();
    assert_eq!(report::publish(&r.f.fs, &r.job, &first).unwrap(), published);
    assert_eq!(std::fs::read(&path).unwrap(), changed);
    assert_eq!(r.job.inspect().unwrap().budget.dispatched_requests, 1);
}
// Pause at the real retained manifest replacement, after build authenticated its
// synthesis and while it owns WriterPermit, but before automatic page apply.
struct PublicationGate {
    reached: std::sync::mpsc::Sender<()>,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    count: std::sync::atomic::AtomicUsize,
    gate_at: usize,
}
impl lwiki::vault::DurableIo for PublicationGate {
    fn create_stage(&self, p: &std::path::Path) -> std::io::Result<std::fs::File> {
        lwiki::vault::DurableIo::create_stage(&lwiki::vault::NativeIo, p)
    }
    fn create_private_stage(&self, p: &std::path::Path) -> std::io::Result<std::fs::File> {
        lwiki::vault::DurableIo::create_private_stage(&lwiki::vault::NativeIo, p)
    }
    fn create_private_directory(&self, p: &std::path::Path) -> std::io::Result<()> {
        lwiki::vault::DurableIo::create_private_directory(&lwiki::vault::NativeIo, p)
    }
    fn open_append(&self, p: &std::path::Path) -> std::io::Result<std::fs::File> {
        lwiki::vault::DurableIo::open_append(&lwiki::vault::NativeIo, p)
    }
    fn truncate_file(&self, f: &std::fs::File, n: u64) -> std::io::Result<()> {
        lwiki::vault::DurableIo::truncate_file(&lwiki::vault::NativeIo, f, n)
    }
    fn write_stage(&self, f: &mut std::fs::File, b: &[u8]) -> std::io::Result<()> {
        lwiki::vault::DurableIo::write_stage(&lwiki::vault::NativeIo, f, b)
    }
    fn sync_file(&self, f: &std::fs::File) -> std::io::Result<()> {
        lwiki::vault::DurableIo::sync_file(&lwiki::vault::NativeIo, f)
    }
    fn replace(&self, a: &std::path::Path, b: &std::path::Path) -> std::io::Result<()> {
        if b.file_name().is_some_and(|name| name == "change.md")
            && self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1 == self.gate_at
        {
            self.reached
                .send(())
                .map_err(|_| std::io::ErrorKind::BrokenPipe)?;
            self.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .map_err(|_| std::io::ErrorKind::TimedOut)?;
        }
        lwiki::vault::DurableIo::replace(&lwiki::vault::NativeIo, a, b)
    }
    fn remove(&self, p: &std::path::Path) -> std::io::Result<()> {
        lwiki::vault::DurableIo::remove(&lwiki::vault::NativeIo, p)
    }
    fn create_directory(&self, p: &std::path::Path) -> std::io::Result<()> {
        lwiki::vault::DurableIo::create_directory(&lwiki::vault::NativeIo, p)
    }
    fn sync_directory(&self, p: &std::path::Path) -> std::io::Result<lwiki::vault::DirectorySync> {
        lwiki::vault::DurableIo::sync_directory(&lwiki::vault::NativeIo, p)
    }
}
#[test]
fn concurrent_epoch_retirement_after_authentication_prevents_page_apply() {
    let r = Reports::new(true, true);
    let validated = r.paid(r.value(true));
    let page = r.f.temp.path().join("pages/known.md");
    let original = std::fs::read(&page).unwrap();
    let (reached_tx, reached_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let fs = lwiki::vault::VaultFs::with_io(
        r.f.fs.root().clone(),
        std::sync::Arc::new(PublicationGate {
            reached: reached_tx,
            release: std::sync::Mutex::new(release_rx),
            count: std::sync::atomic::AtomicUsize::new(0),
            gate_at: 1,
        }),
    );
    let built = std::thread::scope(|threads| {
        let worker = threads.spawn(|| {
            report::build(
                &fs,
                &r.job,
                &r.scope,
                std::slice::from_ref(&r.passage),
                &[],
                Some(validated),
                vec![],
                "publication race",
                false,
            )
        });
        reached_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        r.job
            .pause(StopReason::User("retire synthesis authority".into()))
            .unwrap();
        let i = r.job.inspect().unwrap();
        let research = i.research.as_ref().unwrap();
        let mut binding = research.binding.clone();
        binding.number += 1;
        let rebound = r.job.rebind_research(
            EventPayload::ResearchRebound {
                version: 1,
                expected_epoch: research.binding.number,
                prior_revision: research.frontier_revision,
                amendment_id: Blake3Hash::digest("retire synthesis during first publication"),
                binding,
                active_tasks: vec![],
                tasks: vec![],
                reason: "caller retired synthesis authority".into(),
            },
            &[&r.f.service],
        );
        release_tx.send(()).unwrap();
        rebound.unwrap();
        worker.join().unwrap().unwrap()
    });
    assert!(built.partial);
    assert!(built.gaps.iter().any(|gap| gap.code == "proposal_conflict"));
    assert!(built.proposed_changes.is_empty());
    assert_eq!(std::fs::read(&page).unwrap(), original);
    let inspection = r.job.inspect().unwrap();
    assert_eq!(inspection.research.as_ref().unwrap().binding.number, 1);
    assert!(
        !inspection
            .research
            .as_ref()
            .unwrap()
            .active_tasks
            .contains(&r.task.key)
    );
    assert_eq!(inspection.budget.dispatched_requests, 1);
}
fn two_updates(r: &Reports) -> Value {
    let mut value = r.value(true);
    value["proposed_changes"] = json!(r.records.iter().map(|record| json!({"kind":"update_page","record":record,"body":format!("Unassessed proposal for {}.", record.record_id),"citations":[r.passage.citation]})).collect::<Vec<_>>());
    value
}
fn reopened(r: &Reports) -> JobLedger {
    let mut options = r.f.options.clone();
    options.cancel = CancellationToken::default();
    JobLedger::new(r.f.fs.clone(), id("vault_test"), id("run_reports"), options).unwrap()
}
fn interrupt_before_second_apply(
    r: &Reports,
    validated: ValidatedSynthesis,
) -> lwiki::research::ResearchReport {
    let (reached_tx, reached_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let fs = lwiki::vault::VaultFs::with_io(
        r.f.fs.root().clone(),
        std::sync::Arc::new(PublicationGate {
            reached: reached_tx,
            release: std::sync::Mutex::new(release_rx),
            count: std::sync::atomic::AtomicUsize::new(0),
            gate_at: 2,
        }),
    );
    std::thread::scope(|threads| {
        let worker = threads.spawn(|| {
            report::build(
                &fs,
                &r.job,
                &r.scope,
                std::slice::from_ref(&r.passage),
                &[],
                Some(validated),
                vec![],
                "interrupted publication",
                false,
            )
        });
        reached_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        r.f.options.cancel.cancel();
        release_tx.send(()).unwrap();
        worker.join().unwrap().unwrap()
    })
}
#[test]
fn two_authorized_updates_apply_and_report_without_refreshing_paid_authority() {
    let r = Reports::with_pages(true, 2);
    let validated = r.paid(two_updates(&r));
    let first = r.build(Some(validated.clone()));
    assert!(!first.partial);
    assert_eq!(first.proposed_changes.len(), 2);
    let engine = ChangeEngine::new(r.f.fs.clone()).unwrap();
    for change in &first.proposed_changes {
        let actual = engine.inspect(&change.change_id).unwrap();
        assert_eq!(actual.status, ChangeStatus::Committed);
        let op = &actual.manifest.operations[0];
        assert!(
            r.task
                .source_bindings
                .iter()
                .any(|read| read.path == op.target && read.expected == op.before)
        );
        assert_eq!(
            Blake3Hash::digest(std::fs::read(r.f.fs.root().resolve(&op.target).unwrap()).unwrap()),
            match &op.after {
                lwiki::vault::ExpectedState::Hash(hash) => hash.clone(),
                _ => panic!("page disappeared"),
            }
        );
    }
    let output = report::publish(&r.f.fs, &r.job, &first).unwrap();
    let second = r.build(Some(validated));
    assert_eq!(
        canonical_json(&first).unwrap(),
        canonical_json(&second).unwrap()
    );
    assert_eq!(report::publish(&r.f.fs, &r.job, &second).unwrap(), output);
    assert_eq!(r.job.inspect().unwrap().budget.dispatched_requests, 1);
}
#[test]
fn interrupted_update_publication_resumes_original_drafts_and_refuses_external_edits() {
    for external_edit in [false, true] {
        let r = Reports::with_pages(true, 2);
        let validated = r.paid(two_updates(&r));
        let second_path = r.f.temp.path().join("pages/known2.md");
        let second_original = std::fs::read(&second_path).unwrap();
        let interrupted = interrupt_before_second_apply(&r, validated.clone());
        assert!(interrupted.partial);
        assert_eq!(interrupted.proposed_changes.len(), 1);
        assert_eq!(std::fs::read(&second_path).unwrap(), second_original);
        let first_path = r.f.temp.path().join("pages/known.md");
        let first_applied = std::fs::read(&first_path).unwrap();
        assert!(
            String::from_utf8_lossy(&first_applied).contains("Unassessed proposal for page_known.")
        );
        let edited = String::from_utf8(first_applied)
            .unwrap()
            .replace(
                "Unassessed proposal for page_known.",
                "External user edit after first apply.",
            )
            .into_bytes();
        if external_edit {
            std::fs::write(&first_path, &edited).unwrap();
        }
        let job = reopened(&r);
        let resumed = report::build(
            &r.f.fs,
            &job,
            &r.scope,
            std::slice::from_ref(&r.passage),
            &[],
            Some(validated),
            vec![],
            "resume publication",
            false,
        )
        .unwrap();
        if external_edit {
            assert!(resumed.partial);
            assert!(
                resumed
                    .gaps
                    .iter()
                    .any(|gap| gap.code == "proposal_conflict")
            );
            assert_eq!(std::fs::read(&first_path).unwrap(), edited);
            assert_eq!(std::fs::read(&second_path).unwrap(), second_original);
        } else {
            assert!(!resumed.partial);
            assert_eq!(resumed.proposed_changes.len(), 2);
            assert_eq!(resumed.proposed_changes[0], interrupted.proposed_changes[0]);
            report::publish(&r.f.fs, &job, &resumed).unwrap();
        }
        assert_eq!(job.inspect().unwrap().budget.dispatched_requests, 1);
    }
}
#[test]
fn retired_epoch_cannot_continue_an_interrupted_authorized_update() {
    let r = Reports::with_pages(true, 2);
    let validated = r.paid(two_updates(&r));
    interrupt_before_second_apply(&r, validated.clone());
    let first_path = r.f.temp.path().join("pages/known.md");
    let second_path = r.f.temp.path().join("pages/known2.md");
    let first = std::fs::read(&first_path).unwrap();
    let second = std::fs::read(&second_path).unwrap();
    let job = reopened(&r);
    job.pause(StopReason::User("retire interrupted authority".into()))
        .unwrap();
    let i = job.inspect().unwrap();
    let research = i.research.as_ref().unwrap();
    let mut binding = research.binding.clone();
    binding.number += 1;
    for read in &mut binding.read_preconditions {
        read.expected = lwiki::vault::ExpectedState::Hash(Blake3Hash::digest(
            std::fs::read(r.f.fs.root().resolve(&read.path).unwrap()).unwrap(),
        ));
    }
    job.rebind_research(
        EventPayload::ResearchRebound {
            version: 1,
            expected_epoch: research.binding.number,
            prior_revision: research.frontier_revision,
            amendment_id: Blake3Hash::digest("retire interrupted page authority"),
            binding,
            active_tasks: vec![],
            tasks: vec![],
            reason: "caller retired interrupted synthesis".into(),
        },
        &[&r.f.service],
    )
    .unwrap();
    assert!(
        report::build(
            &r.f.fs,
            &job,
            &r.scope,
            std::slice::from_ref(&r.passage),
            &[],
            Some(validated),
            vec![],
            "retired continuation",
            false
        )
        .is_err()
    );
    assert_eq!(std::fs::read(&first_path).unwrap(), first);
    assert_eq!(std::fs::read(&second_path).unwrap(), second);
    assert_eq!(job.inspect().unwrap().budget.dispatched_requests, 1);
}
#[test]
fn stale_citations_are_omitted_and_claims_move_to_unassessed_gaps() {
    let r = Reports::new(false, false);
    let validated = r.paid(r.value(false));
    let plan = SourceStore::new(r.f.fs.clone())
        .plan_refresh(
            &r.f.packet.source_id,
            CaptureRequest {
                title: "Synthetic API source".into(),
                origin_kind: SourceOrigin::LocalFile,
                origin: "fixture.md".into(),
                original: b"Changed source bytes.".to_vec(),
                extraction: ExtractionInput::Utf8Preserve,
                media_type: None,
            },
        )
        .unwrap();
    let engine = ChangeEngine::new(r.f.fs.clone()).unwrap();
    let writer = WriterPermit::acquire(r.f.fs.root(), Duration::from_secs(2)).unwrap();
    let prepared = engine
        .prepare(&writer, plan.draft.unwrap())
        .unwrap()
        .prepared;
    engine
        .apply(
            &writer,
            &prepared,
            &CatalogGraphValidator,
            &Catalog::new(r.f.fs.clone(), id("vault_test")),
        )
        .unwrap();
    drop(writer);
    let report = r.build(Some(validated));
    assert!(report.passages.is_empty());
    assert!(report.proposed_changes.is_empty());
    assert!(report.gaps.iter().any(|g| g.code == "stale_passage"));
    assert!(
        report
            .gaps
            .iter()
            .any(|g| g.code == "stale_claim" && g.message.contains("unsupported conclusion"))
    );
    assert!(report.gaps.iter().any(|g| g.code == "stale_proposal"));
    assert!(
        report.synthesis.unwrap().sections[0]
            .claims
            .iter()
            .all(|c| c.citations.is_empty())
    );
}
#[test]
fn unknown_records_invented_paths_and_forged_retained_hashes_fail_closed() {
    let r = Reports::new(false, true);
    let value = r.value(true);
    let view = SourceView::from_fs_bounded(&r.f.fs, 64 * 1024 * 1024, 4096).unwrap();
    let mut bad = value.clone();
    bad["proposed_changes"][0]["path"] = json!("WIKI.md");
    assert!(
        synthesis::validate(
            &canonical_json(&bad).unwrap(),
            &r.scope.limits.stage,
            std::slice::from_ref(&r.passage.citation),
            &r.records,
            &view
        )
        .is_err()
    );
    let mut validated = r.paid(value);
    let genuine = r.build(Some(validated.clone()));
    let mut forged_hash = genuine.proposed_changes[0].clone();
    forged_hash.manifest_hash = Blake3Hash::digest("forged");
    assert!(
        report::build(
            &r.f.fs,
            &r.job,
            &r.scope,
            &[],
            &[],
            None,
            vec![forged_hash],
            "partial",
            true
        )
        .is_err()
    );
    let synthesis::ResearchProposal::UpdatePage { record, .. } =
        &mut validated.output.proposed_changes[0]
    else {
        unreachable!()
    };
    record.record_id = id("page_unknown");
    assert!(
        report::build(
            &r.f.fs,
            &r.job,
            &r.scope,
            std::slice::from_ref(&r.passage),
            &[],
            Some(validated),
            vec![],
            "coverage",
            false
        )
        .is_err()
    );
    let forged = PreparedChange {
        change_id: id("change_missing"),
        manifest_hash: Blake3Hash::digest("forged"),
    };
    assert!(
        report::build(
            &r.f.fs,
            &r.job,
            &r.scope,
            &[],
            &[],
            None,
            vec![forged],
            "partial",
            true
        )
        .is_err()
    );
}
#[test]
fn dry_run_is_read_only_and_offline_partial_publication_needs_no_model() {
    let r = Reports::new(false, false);
    r.job.pause(StopReason::Budget).unwrap();
    let mut options = r.f.options.clone();
    options.policy.dry_run = true;
    let dry = JobLedger::new(r.f.fs.clone(), id("vault_test"), id("run_reports"), options).unwrap();
    let before = provider::tree(r.f.temp.path());
    let first = report::build(
        &r.f.fs,
        &dry,
        &r.scope,
        std::slice::from_ref(&r.passage),
        &[],
        None,
        vec![],
        "budget",
        true,
    )
    .unwrap();
    let second = report::build(
        &r.f.fs,
        &dry,
        &r.scope,
        std::slice::from_ref(&r.passage),
        &[],
        None,
        vec![],
        "budget",
        true,
    )
    .unwrap();
    assert_eq!(
        canonical_json(&first).unwrap(),
        canonical_json(&second).unwrap()
    );
    assert!(report::publish(&r.f.fs, &dry, &first).is_err());
    assert_eq!(provider::tree(r.f.temp.path()), before);
    let mut options = r.f.options.clone();
    options.policy.offline = true;
    options.cancel.cancel();
    let offline =
        JobLedger::new(r.f.fs.clone(), id("vault_test"), id("run_reports"), options).unwrap();
    let out = report::publish(&r.f.fs, &offline, &first).unwrap();
    assert!(r.f.fs.root().resolve(&out.path).unwrap().exists());
    assert_eq!(offline.inspect().unwrap().budget.dispatched_requests, 0);
    assert!(
        report::latest(&r.f.fs, &offline.inspect().unwrap())
            .unwrap()
            .unwrap()
            .partial
    );
}
#[test]
fn newest_report_uses_actual_admission_order_and_checks_output_ownership() {
    let r = Reports::new(false, false);
    r.job.pause(StopReason::Budget).unwrap();
    let first = report::build(
        &r.f.fs,
        &r.job,
        &r.scope,
        &[],
        &[],
        None,
        vec![],
        "first",
        true,
    )
    .unwrap();
    report::publish(&r.f.fs, &r.job, &first).unwrap();
    let first_hash = Blake3Hash::digest(canonical_json(&first).unwrap());
    let mut second = None;
    for index in 0..100 {
        let candidate = report::build(
            &r.f.fs,
            &r.job,
            &r.scope,
            &[],
            &[],
            None,
            vec![],
            &format!("later_{index}"),
            true,
        )
        .unwrap();
        if Blake3Hash::digest(canonical_json(&candidate).unwrap()) < first_hash {
            second = Some(candidate);
            break;
        }
    }
    let second = second.expect("fixture must choose reverse lexical report hash order");
    let output = report::publish(&r.f.fs, &r.job, &second).unwrap();
    let inspected = r.job.inspect().unwrap();
    assert_eq!(
        report::latest(&r.f.fs, &inspected)
            .unwrap()
            .unwrap()
            .stop_reason,
        second.stop_reason
    );
    let mut forged = inspected.clone();
    let task = forged
        .tasks
        .values_mut()
        .find(|t| t.outputs.contains(&output))
        .unwrap();
    task.outputs[0].record.vault_id = id("vault_other");
    assert!(report::latest(&r.f.fs, &forged).is_err());
    let bytes = std::fs::read(r.f.fs.root().resolve(&output.path).unwrap()).unwrap();
    std::fs::write(
        r.f.fs.root().resolve(&output.path).unwrap(),
        [bytes, b"tampered".to_vec()].concat(),
    )
    .unwrap();
    assert!(report::latest(&r.f.fs, &inspected).is_err());
}

#[test]
fn stale_epoch_partial_report_preserves_paid_hold_and_cannot_authorize_new_send() {
    let r = Reports::new(false, false);
    let _paid = r.paid(r.value(false));
    let pending = plan::generation_task(
        &id("run_reports"),
        &r.scope,
        TaskStage::AssessGaps,
        1,
        &[],
        std::slice::from_ref(&r.passage),
        &[],
        &r.f.service,
        10,
        vec![],
    )
    .unwrap();
    std::fs::write(
        r.f.fs.root().resolve(&pending.task.input.path).unwrap(),
        &pending.bytes,
    )
    .unwrap();
    let before = r.job.inspect().unwrap();
    let research = before.research.as_ref().unwrap();
    r.job
        .admit_research_frontier(EventPayload::ResearchFrontierAdmitted {
            version: 1,
            epoch: research.binding.number,
            prior_revision: research.frontier_revision,
            admission_id: Blake3Hash::digest("pending-before-edit"),
            round: 0,
            origins: vec![],
            tasks: vec![pending.task.clone()],
            task_origins: vec![],
            parent_outputs: vec![],
        })
        .unwrap();
    r.job.pause(StopReason::Budget).unwrap();
    let original = r.job.inspect().unwrap();
    assert!(
        original
            .attempts
            .iter()
            .any(|a| a.billing == BillingDisposition::UnknownReserved)
    );
    let capture = SourceStore::new(r.f.fs.clone())
        .plan_refresh(
            &r.f.packet.source_id,
            CaptureRequest {
                title: "Synthetic API source".into(),
                origin_kind: SourceOrigin::LocalFile,
                origin: "fixture.md".into(),
                original: b"Edited current source.".to_vec(),
                extraction: ExtractionInput::Utf8Preserve,
                media_type: None,
            },
        )
        .unwrap();
    let engine = ChangeEngine::new(r.f.fs.clone()).unwrap();
    let writer = WriterPermit::acquire(r.f.fs.root(), Duration::from_secs(2)).unwrap();
    let prepared = engine
        .prepare(&writer, capture.draft.unwrap())
        .unwrap()
        .prepared;
    engine
        .apply(
            &writer,
            &prepared,
            &CatalogGraphValidator,
            &Catalog::new(r.f.fs.clone(), id("vault_test")),
        )
        .unwrap();
    drop(writer);
    let partial = report::build(
        &r.f.fs,
        &r.job,
        &r.scope,
        std::slice::from_ref(&r.passage),
        &[],
        None,
        vec![],
        "source_changed",
        true,
    )
    .unwrap();
    assert!(partial.passages.is_empty());
    assert!(partial.gaps.iter().any(|g| g.code == "stale_passage"));
    report::publish(&r.f.fs, &r.job, &partial).unwrap();
    let after = r.job.inspect().unwrap();
    assert_eq!(after.spec_hash, original.spec_hash);
    assert!(after.budget.journal_bytes_used > original.budget.journal_bytes_used);
    assert_eq!(
        after.budget.journal_events_used,
        original.budget.journal_events_used + 2
    );
    let mut expected_budget = original.budget.clone();
    expected_budget.journal_bytes_used = after.budget.journal_bytes_used;
    expected_budget.journal_events_used = after.budget.journal_events_used;
    assert_eq!(after.budget, expected_budget);
    assert_eq!(
        after.research.as_ref().unwrap().binding,
        original.research.as_ref().unwrap().binding
    );
    assert_eq!(after.attempts, original.attempts);
    let mock = Mock::response(json!({}));
    let dispatcher = r.f.dispatcher(mock.clone());
    let failure = dispatcher
        .execute(
            &r.job,
            &r.f.service,
            &pending.task.key,
            DispatchPurpose::Task,
        )
        .err()
        .unwrap();
    assert_eq!(failure.error.code, ErrorCode::FreshnessConflict);
    assert_eq!(mock.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(!dispatcher.network_used());
    assert_eq!(r.job.inspect().unwrap().budget, after.budget);
}

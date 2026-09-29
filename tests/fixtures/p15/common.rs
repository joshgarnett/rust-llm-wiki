use super::lwiki::{
    domain::*,
    jobs::{self, *},
    vault::{VaultFs, VaultRoot, WriterPermit},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
    time::Duration,
};
pub struct Clock(pub AtomicI64);
impl JobClock for Clock {
    fn read(&self) -> Result<ClockReading> {
        Ok(ClockReading {
            utc_ms: self.0.load(Ordering::SeqCst),
            monotonic_ms: 10,
        })
    }
}
pub fn hash(v: &str) -> Blake3Hash {
    Blake3Hash::digest(v)
}
pub fn id(v: &str) -> RecordId {
    RecordId::new(v).unwrap()
}
pub fn rel(v: &str) -> VaultRelativePath {
    VaultRelativePath::new(v).unwrap()
}
pub fn options(clock: Arc<Clock>) -> JobOptions {
    JobOptions {
        clock,
        fault: None,
        cancel: CancellationToken::default(),
        policy: ExecutionPolicy::default(),
        lock_timeout_ms: 5000,
    }
}
pub fn spec(fs: &VaultFs, run: &str, count: usize) -> RunSpec {
    let mut tasks = vec![];
    for n in 0..count {
        let path = rel(&format!("inputs/task{n}.json"));
        let data = format!("{{\"input\":{n}}}").into_bytes();
        std::fs::create_dir_all(fs.root().path().join("inputs")).unwrap();
        std::fs::write(fs.root().resolve(&path).unwrap(), &data).unwrap();
        let h = Blake3Hash::digest(&data);
        let mut task = TaskSpec {
            key: hash("unassigned"),
            stage: TaskStage::Extract,
            capability: Some(Capability::Generate),
            priority: n as i32,
            dependencies: vec![],
            input_hash: h.clone(),
            prompt_hash: Some(hash("prompt")),
            schema_hash: Some(hash("schema")),
            model_hash: Some(hash("model")),
            settings_hash: hash("settings"),
            source_bindings: vec![],
            input: BoundedPayloadRef {
                path,
                hash: h,
                byte_len: data.len() as u64,
            },
        };
        task.key = jobs::tasks::task_key(&task).unwrap();
        tasks.push(task)
    }
    let mut s = RunSpec {
        version: 1,
        run_id: id(run),
        vault_id: id("vault_test"),
        title: "Accounted mock run".into(),
        created_at_utc_ms: 1_700_000_000_000,
        deadline_utc_ms: 1_700_000_900_000,
        scope: RunScope {
            operation: "extract".into(),
            question: None,
            exclusions: vec![],
            source_snapshot: None,
            input_records: vec![],
            read_preconditions: vec![],
            profile_fingerprints: BTreeMap::from([("mock".into(), hash("profile"))]),
            scope_payload_hash: None,
        },
        config_fingerprint: hash("config"),
        input_fingerprint: hash("unassigned"),
        limits: LifetimeLimits::default(),
        tasks,
        prior_accounting: PriorAccounting::None,
    };
    s.input_fingerprint = jobs::tasks::input_fingerprint(&s).unwrap();
    s
}
pub fn bound(task: &TaskSpec) -> AttemptBound {
    let mut b = AttemptBound {
        codec: None,
        profile_fingerprint: None,
        capability: Capability::Generate,
        profile_id: "mock".into(),
        endpoint_fingerprint: hash("endpoint"),
        config_fingerprint: hash("config"),
        input_hash: task.input_hash.clone(),
        wire_hash: hash(&format!("wire-{}", task.key)),
        requested_model: Some("mock-model".into()),
        requested_model_revision: None,
        request_bytes: 100,
        response_bytes: 1000,
        timeout_ms: 10000,
        applicable_classes: BTreeSet::from([
            BillableClass::Input,
            BillableClass::Output,
            BillableClass::Reasoning,
        ]),
        billable_bounds: [
            (BillableClass::Input, 100),
            (BillableClass::Output, 100),
            (BillableClass::Reasoning, 50),
        ]
        .into_iter()
        .map(|(c, count)| {
            (
                c,
                TokenBound::ProvenUpper {
                    count,
                    method: "mock mathematical bound".into(),
                    fingerprint: hash("bound"),
                },
            )
        })
        .collect(),
        rate_card: None,
        quoted_allowance: None,
        bounds_fingerprint: hash("unassigned"),
    };
    b.bounds_fingerprint = jobs::budgets::bound_fingerprint(&b).unwrap();
    b
}
pub fn fixture(
    count: usize,
    mut change: impl FnMut(&mut RunSpec),
) -> (tempfile::TempDir, VaultFs, JobLedger, RunSpec, Arc<Clock>) {
    let t = tempfile::tempdir().unwrap();
    std::fs::write(
        t.path().join("WIKI.md"),
        b"---\nwiki_schema: \"1\"\nwiki_id: vault_test\nwiki_kind: vault\ntitle: Jobs\n---\n",
    )
    .unwrap();
    let fs = VaultFs::new(VaultRoot::explicit(t.path()).unwrap());
    let mut spec = spec(&fs, "run_test", count);
    change(&mut spec);
    spec.input_fingerprint = jobs::tasks::input_fingerprint(&spec).unwrap();
    let clock = Arc::new(Clock(AtomicI64::new(spec.created_at_utc_ms)));
    let job = JobLedger::new(
        fs.clone(),
        spec.vault_id.clone(),
        spec.run_id.clone(),
        options(clock.clone()),
    )
    .unwrap();
    let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(1)).unwrap();
    job.create(&writer, spec.clone()).unwrap();
    drop(writer);
    job.start().unwrap();
    (t, fs, job, spec, clock)
}

pub fn priced_bound(task: &TaskSpec) -> AttemptBound {
    let mut b = bound(task);
    let mut card = RateCard {
        id: "mock-rates".into(),
        version: 1,
        fingerprint: hash("unassigned"),
        currency: Currency::new("USD").unwrap(),
        validity: PriceValidity::DispatchLocked {
            valid_from_utc_ms: 1_699_999_999_000,
            valid_until_utc_ms: 1_700_000_900_000,
        },
        request_fee_nanounits: 1,
        rates: b
            .applicable_classes
            .iter()
            .map(|c| {
                (
                    *c,
                    Rate {
                        price_nanounits: 0,
                        per_units: std::num::NonZeroU64::new(1).unwrap(),
                    },
                )
            })
            .collect(),
    };
    card.fingerprint = jobs::budgets::rate_card_fingerprint(&card).unwrap();
    b.rate_card = Some(card);
    b.quoted_allowance = Some(Money::new(Currency::new("USD").unwrap(), 1));
    b.bounds_fingerprint = jobs::budgets::bound_fingerprint(&b).unwrap();
    b
}

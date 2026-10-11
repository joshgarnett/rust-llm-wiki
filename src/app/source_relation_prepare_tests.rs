use super::*;
use crate::{
    app::{OperationOptions, source_relation_inputs::tests::Fixture},
    sources::selected::verify_captured_source,
};
use std::time::Duration;
fn until() -> Instant {
    Instant::now() + Duration::from_secs(60)
}
fn fixture() -> (Fixture, OfflineApp) {
    let f = Fixture::new(false);
    for (path, bytes) in &f.captured {
        let actual = f.fs.root().resolve(path).unwrap();
        std::fs::create_dir_all(actual.parent().unwrap()).unwrap();
        std::fs::write(actual, bytes).unwrap();
    }
    let app = OfflineApp::new(f.fs.clone(), OperationOptions::default()).unwrap();
    app.index_rebuild_normalized().unwrap();
    (f, app)
}
#[test]
fn selected_capture_authenticates_all_original_bytes_without_unrelated_body_reads() {
    let (f, app) = fixture();
    let unrelated = f.fs.root().path().join("unrelated-content.md");
    let file = std::fs::File::create(&unrelated).unwrap();
    file.set_len(2 * 1024 * 1024).unwrap();
    let catalog = app.catalog();
    let reader = catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let expected = f.authenticate().unwrap().selected_binding;
    let mut remaining = TOTAL_CAPTURE_BYTES;
    let (binding, captured) = capture_source(
        &f.fs,
        &reader,
        &f.vault,
        &expected.source_id,
        &mut remaining,
        until(),
    )
    .unwrap();
    assert_eq!(binding, expected);
    assert_eq!(captured, f.captured);
    assert_eq!(
        TOTAL_CAPTURE_BYTES - remaining,
        captured.values().map(Vec::len).sum::<usize>()
    );
    verify_captured_source(&f.fs, &binding, &captured).unwrap();
    reader.verify_operations(&catalog).unwrap();
    assert_eq!(std::fs::metadata(unrelated).unwrap().len(), 2 * 1024 * 1024);
}
#[test]
fn duplicate_missing_oversize_and_expired_inputs_refuse_without_partial_result() {
    let (f, app) = fixture();
    let binding = f.authenticate().unwrap().selected_binding;
    assert!(
        unique_ids(
            &[binding.source_id.clone(), binding.source_id.clone()],
            1024,
            until()
        )
        .is_err()
    );
    assert!(unique_ids(&vec![binding.source_id.clone(); 4097], 1024, until()).is_err());
    assert!(
        unique_ids(
            std::slice::from_ref(&binding.source_id),
            1024,
            Instant::now()
        )
        .is_err()
    );
    let catalog = app.catalog();
    let reader = catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let mut remaining = TOTAL_CAPTURE_BYTES;
    assert!(
        capture_source(
            &f.fs,
            &reader,
            &f.vault,
            &RecordId::new("fictional_missing").unwrap(),
            &mut remaining,
            until()
        )
        .is_err()
    );
    assert_eq!(remaining, TOTAL_CAPTURE_BYTES);
    let original = f
        .captured
        .keys()
        .find(|p| p.as_str().ends_with("original.bin"))
        .unwrap();
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(f.fs.root().resolve(original).unwrap())
        .unwrap();
    file.set_len(MAX_CAPTURED_BYTES as u64 + 1).unwrap();
    assert!(
        capture_source(
            &f.fs,
            &reader,
            &f.vault,
            &binding.source_id,
            &mut remaining,
            until()
        )
        .is_err()
    );
}
#[test]
fn stale_catalog_source_and_final_guard_change_are_refused() {
    let (f, app) = fixture();
    let binding = f.authenticate().unwrap().selected_binding;
    let catalog = app.catalog();
    let reader = catalog
        .cached_query_snapshot(QueryReadLimits::default())
        .unwrap();
    let mut remaining = TOTAL_CAPTURE_BYTES;
    let (_, captured) = capture_source(
        &f.fs,
        &reader,
        &f.vault,
        &binding.source_id,
        &mut remaining,
        until(),
    )
    .unwrap();
    let guards = captured
        .iter()
        .map(|(p, b)| (p.clone(), (Blake3Hash::digest(b), b.len())))
        .collect();
    let mut changed = captured[&binding.source_path].clone();
    // Syntactically unchanged control envelope, distinct exact bytes.
    changed.push(b'\n');
    std::fs::write(f.fs.root().resolve(&binding.source_path).unwrap(), changed).unwrap();
    assert!(verify_guards(&f.fs, guards, &mut remaining, until()).is_err());
    assert_eq!(
        capture_source(
            &f.fs,
            &reader,
            &f.vault,
            &binding.source_id,
            &mut remaining,
            until()
        )
        .err()
        .unwrap()
        .code,
        ErrorCode::FreshnessConflict
    );
}
#[test]
fn cumulative_allowance_is_enforced_before_large_capture_allocation() {
    let (f, _) = fixture();
    let content = f.content.clone();
    let mut remaining = 1;
    assert!(read(&f.fs, &content, MAX_CONTENT_BYTES, &mut remaining, until()).is_err());
    assert_eq!(remaining, 1);
}

use crate as library;
#[allow(dead_code)]
#[allow(
    clippy::duplicate_mod,
    reason = "Instantiate shared fixture code in independent test namespaces; sharing module state would change the test contract."
)]
#[path = "../../tests/fixtures/p16c/common.rs"]
mod quote_fixture;
use crate::{
    config::providers::ProviderConfig,
    jobs::{BillableClass, ClockReading, Currency, JobClock, Money, PriceValidity, Rate, RateCard},
    providers::{
        credentials::{CredentialBroker, CredentialOptions},
        dispatcher::Dispatcher,
        types::DispatchOptions,
    },
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

struct QuoteSetup {
    s: crate::app::source_relation_preparation::tests::Setup,
    _config: tempfile::TempDir,
    path: PathBuf,
    secrets: Arc<quote_fixture::Inputs>,
    helpers: Arc<quote_fixture::Runner>,
}
fn priced_setup(count: usize) -> QuoteSetup {
    let mut s = crate::app::source_relation_preparation::tests::setup(count);
    let config = tempfile::tempdir().unwrap();
    let path = config.path().join("providers.toml");
    let now = s.runtime.created_at_utc_ms;
    let mut card = RateCard {
        id: "mini-quote-fixture".into(),
        version: 1,
        fingerprint: Blake3Hash::digest([]),
        currency: Currency::new("USD").unwrap(),
        validity: PriceValidity::DispatchLocked {
            valid_from_utc_ms: 0,
            valid_until_utc_ms: now + 100_000,
        },
        request_fee_nanounits: 6_000_000_000,
        rates: [
            (BillableClass::Input, 400_000_000),
            (BillableClass::CachedInput, 100_000_000),
            (BillableClass::Output, 1_600_000_000),
            (BillableClass::Reasoning, 0),
        ]
        .into_iter()
        .map(|(c, n)| {
            (
                c,
                Rate {
                    price_nanounits: n,
                    per_units: std::num::NonZeroU64::new(1_000_000).unwrap(),
                },
            )
        })
        .collect(),
    };
    card.fingerprint = crate::jobs::budgets::rate_card_fingerprint(&card).unwrap();
    let root = serde_json::to_string(s.source.fs.root().path().to_str().unwrap()).unwrap();
    let mut text = format!(
        "version=1\n[profiles.primary]\ngeneration=\"generate\"\n[services.generate]\nadapter=\"responses-v1\"\nurl=\"https://api.openai.com/v1/responses\"\nmodel=\"gpt-4.1-mini-2025-04-14\"\nrevision=\"gpt-4.1-mini-2025-04-14\"\nmax_output_tokens=4096\ntimeout_seconds=1\nconnect_timeout_seconds=1\n[services.generate.auth]\nkind=\"static\"\nkey_env=\"FICTIONAL_MOCK_ONLY\"\n[services.generate.rate_card]\nid=\"mini-quote-fixture\"\nversion=1\nfingerprint={}\ncurrency=\"USD\"\nrequest_fee_nanounits=6000000000\n[services.generate.rate_card.validity]\npricing=\"dispatch_locked\"\nvalid_from_utc_ms=0\nvalid_until_utc_ms={}\n",
        serde_json::to_string(card.fingerprint.as_str()).unwrap(),
        now + 100_000
    );
    for (c, r) in &card.rates {
        let name = match c {
            BillableClass::Input => "input",
            BillableClass::CachedInput => "cached_input",
            BillableClass::Output => "output",
            BillableClass::Reasoning => "reasoning",
        };
        text.push_str(&format!(
            "[services.generate.rate_card.rates.{name}]\nprice_nanounits={}\nper_units={}\n",
            r.price_nanounits, r.per_units
        ));
    }
    text.push_str(&format!("[vault_bindings.main]\nroot={root}\nwiki_id=\"fictional_vault\"\nallowed_profiles=[\"primary\"]\n"));
    quote_fixture::private(&path, text);
    s.runtime.service = ProviderConfig::load(&path)
        .unwrap()
        .authorize(
            &s.source.fs,
            &s.source.vault,
            "primary",
            crate::jobs::Capability::Generate,
        )
        .unwrap();
    s.runtime.limits.max_cost = Some(Money::new(Currency::new("USD").unwrap(), 10_000_000_000));
    let secrets = Arc::new(quote_fixture::Inputs(AtomicUsize::new(0)));
    let helpers = Arc::new(quote_fixture::Runner {
        calls: AtomicUsize::new(0),
        hook: None,
    });
    let broker = Arc::new(CredentialBroker::new(CredentialOptions {
        clock: s.runtime.job_options.clock.clone(),
        inputs: secrets.clone(),
        runner: helpers.clone(),
    }));
    s.runtime.dispatcher = Dispatcher::new(
        s.source.fs.clone(),
        DispatchOptions {
            broker,
            transport: s.mock.clone(),
            jitter: Arc::new(quote_fixture::ZeroJitter),
        },
    );
    s.app.index_rebuild_normalized().unwrap();
    QuoteSetup {
        s,
        _config: config,
        path,
        secrets,
        helpers,
    }
}
fn ids(q: &QuoteSetup) -> Vec<RecordId> {
    q.s.inputs
        .iter()
        .map(|(b, _)| b.source_id.clone())
        .collect()
}

// Compare every fixture path and content; SQLite reader coordination may update
// the existing catalog SHM mtime. Keep its bytes and identity strictly checked.
#[derive(Debug, PartialEq, Eq)]
struct QuoteFileEvidence {
    bytes: u64,
    directory: bool,
    readonly: bool,
    #[cfg(unix)]
    identity: (u64, u64, u32),
    content: Option<Blake3Hash>,
    modified: Option<std::time::SystemTime>,
}
fn quote_tree(root: &std::path::Path) -> BTreeMap<String, QuoteFileEvidence> {
    fn walk(
        root: &std::path::Path,
        path: &std::path::Path,
        out: &mut BTreeMap<String, QuoteFileEvidence>,
    ) {
        let mut entries = std::fs::read_dir(path)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect::<Vec<_>>();
        entries.sort();
        for path in entries {
            let metadata = std::fs::symlink_metadata(&path).unwrap();
            assert!(
                !metadata.file_type().is_symlink(),
                "unexpected fixture symlink: {}",
                path.display()
            );
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            let catalog_shm = path.parent() == Some(root.join(".wiki/cache/catalogs").as_path())
                && relative.ends_with(".sqlite-shm")
                && metadata.is_file();
            #[cfg(unix)]
            let identity = {
                use std::os::unix::fs::MetadataExt;
                (metadata.dev(), metadata.ino(), metadata.mode())
            };
            out.insert(
                relative,
                QuoteFileEvidence {
                    bytes: metadata.len(),
                    directory: metadata.is_dir(),
                    readonly: metadata.permissions().readonly(),
                    #[cfg(unix)]
                    identity,
                    content: if metadata.is_dir() {
                        None
                    } else {
                        Some(Blake3Hash::digest(std::fs::read(&path).unwrap()))
                    },
                    modified: if catalog_shm {
                        None
                    } else {
                        Some(metadata.modified().unwrap())
                    },
                },
            );
            if metadata.is_dir() {
                walk(root, &path, out);
            }
        }
    }
    let mut result = BTreeMap::new();
    walk(root, root, &mut result);
    result
}
fn assert_quote_tree_unchanged(
    before: &BTreeMap<String, QuoteFileEvidence>,
    after: &BTreeMap<String, QuoteFileEvidence>,
) {
    let paths: BTreeSet<_> = before.keys().chain(after.keys()).collect();
    let changed = paths
        .into_iter()
        .filter(|p| before.get(*p) != after.get(*p))
        .collect::<Vec<_>>();
    let sample = changed
        .iter()
        .take(8)
        .map(|p| {
            format!(
                "{p}: before={:?}, after={:?}",
                before.get(*p),
                after.get(*p)
            )
        })
        .collect::<Vec<_>>();
    assert!(
        changed.is_empty(),
        "quote mutated {} vault entries (first 8): {}",
        changed.len(),
        sample.join("; ")
    );
}

fn untouched(q: &QuoteSetup) {
    assert_eq!(q.s.mock.calls.load(Ordering::SeqCst), 0);
    assert_eq!(q.secrets.0.load(Ordering::SeqCst), 0);
    assert_eq!(q.helpers.calls.load(Ordering::SeqCst), 0);
}
#[test]
fn real_two_owner_quote_preserves_manifest_reports_serial_cap_and_has_no_side_effects() {
    let q = priced_setup(2);
    let before = quote_tree(q.s.source.fs.root().path());
    let plain =
        q.s.app
            .prepare_source_relations(&ids(&q), &q.s.runtime, 1024, false, until())
            .unwrap();
    let quoted =
        q.s.app
            .prepare_source_relations(&ids(&q), &q.s.runtime, 1024, true, until())
            .unwrap();
    assert_eq!(plain.manifest, quoted.manifest);
    let quote = quoted.quote.unwrap();
    assert_eq!(quote.manifest_hash, Blake3Hash::digest(&plain.manifest));
    assert_eq!(quote.entries.len(), 2);
    assert!(!quote.aggregate_fits_lifetime_limits);
    assert!(!quote.complete_acquisition_guaranteed);
    assert_eq!(quote.bound_identity_stage, "prepared_wire");
    assert!(
        quote
            .aggregate_worst_case
            .cost
            .as_ref()
            .unwrap()
            .nanounits()
            > 10_000_000_000
    );
    let manifest: SourceRelationManifest = serde_json::from_slice(&plain.manifest).unwrap();
    for (entry, planned) in quote.entries.iter().zip(&manifest.entries) {
        assert_eq!(entry.source_id, planned.source.source_id);
        assert_eq!(entry.task_key, planned.task.key);
        assert_eq!(entry.input_descriptor, planned.task.input);
        assert_eq!(entry.bound.input_hash, planned.task.input_hash);
        assert!(entry.bound.request_bytes > 0);
        assert_eq!(entry.bound.codec, None);
        assert_eq!(entry.bound.profile_fingerprint, None);
        crate::jobs::budgets::fits(
            &crate::jobs::budgets::zero(),
            &crate::jobs::budgets::zero(),
            &entry.allowance,
            &q.s.runtime.limits,
        )
        .unwrap();
    }
    untouched(&q);
    assert_quote_tree_unchanged(&before, &quote_tree(q.s.source.fs.root().path()));
}
struct QuoteClock {
    base: i64,
    count: AtomicUsize,
    at: usize,
    jump: i64,
    hook: Option<Arc<dyn Fn() + Send + Sync>>,
}
impl JobClock for QuoteClock {
    fn read(&self) -> Result<ClockReading> {
        let n = self.count.fetch_add(1, Ordering::SeqCst) + 1;
        if n == self.at
            && let Some(h) = &self.hook
        {
            h();
        }
        Ok(ClockReading {
            utc_ms: self.base + if n >= self.at { self.jump } else { 0 },
            monotonic_ms: n as u64,
        })
    }
}
#[test]
fn rates_expiring_during_second_capture_refuse_entire_quote_without_writes() {
    let mut q = priced_setup(2);
    q.s.runtime.job_options.clock = Arc::new(QuoteClock {
        base: q.s.runtime.created_at_utc_ms,
        count: AtomicUsize::new(0),
        at: 5,
        jump: 200_000,
        hook: None,
    });
    let before = quote_tree(q.s.source.fs.root().path());
    assert!(
        q.s.app
            .prepare_source_relations(&ids(&q), &q.s.runtime, 1024, true, until())
            .is_err()
    );
    untouched(&q);
    assert_quote_tree_unchanged(&before, &quote_tree(q.s.source.fs.root().path()));
}
#[test]
fn final_config_mutation_is_refused_after_common_clock_hook() {
    let mut q = priced_setup(2);
    let path = q.path.clone();
    let hook = Arc::new(move || {
        let mut text = std::fs::read(&path).unwrap();
        text.extend_from_slice(b"\n# configuration changed during final quote\n");
        quote_fixture::private(&path, text);
    });
    let clock = Arc::new(QuoteClock {
        base: q.s.runtime.created_at_utc_ms,
        count: AtomicUsize::new(0),
        at: 8,
        jump: 0,
        hook: Some(hook),
    });
    q.s.runtime.job_options.clock = clock.clone();
    assert!(
        q.s.app
            .prepare_source_relations(&ids(&q), &q.s.runtime, 1024, true, until())
            .is_err()
    );
    assert!(clock.count.load(Ordering::SeqCst) >= 8);
    untouched(&q);
}

#[test]
fn source_mutation_during_later_wire_quote_refuses_complete_result() {
    let mut q = priced_setup(2);
    let path =
        q.s.source
            .fs
            .root()
            .resolve(&q.s.inputs[0].0.content_path)
            .unwrap();
    let hook = Arc::new(move || {
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[0] ^= 1;
        std::fs::write(&path, bytes).unwrap();
    });
    let clock = Arc::new(QuoteClock {
        base: q.s.runtime.created_at_utc_ms,
        count: AtomicUsize::new(0),
        at: 5,
        jump: 0,
        hook: Some(hook),
    });
    q.s.runtime.job_options.clock = clock.clone();
    assert!(
        q.s.app
            .prepare_source_relations(&ids(&q), &q.s.runtime, 1024, true, until())
            .is_err()
    );
    assert!(clock.count.load(Ordering::SeqCst) >= 5);
    untouched(&q);
}
#[test]
fn quote_duplicate_missing_and_individual_cost_refuse_complete_preparation() {
    let mut q = priced_setup(2);
    let selected = ids(&q);
    assert!(
        q.s.app
            .prepare_source_relations(
                &[selected[0].clone(), selected[0].clone()],
                &q.s.runtime,
                1024,
                true,
                until()
            )
            .is_err()
    );
    assert!(
        q.s.app
            .prepare_source_relations(
                &[
                    selected[0].clone(),
                    RecordId::new("fictional_absent").unwrap()
                ],
                &q.s.runtime,
                1024,
                true,
                until()
            )
            .is_err()
    );
    q.s.runtime.limits.max_cost = Some(Money::new(Currency::new("USD").unwrap(), 5_000_000_000));
    assert!(
        q.s.app
            .prepare_source_relations(&selected, &q.s.runtime, 1024, true, until())
            .is_err()
    );
    untouched(&q);
}
#[test]
fn quote_builder_rejects_empty_duplicate_poison_and_changed_service_identity() {
    use crate::app::{
        source_relation_inputs::InputLimits, source_relation_planning::plan_captured,
        source_relation_quote::QuoteBuilder,
    };
    let mut q = priced_setup(1);
    let deadline = until();
    let b = QuoteBuilder::new(&q.s.runtime, deadline).unwrap();
    assert!(
        b.finish(&q.s.runtime, Blake3Hash::digest([]), deadline)
            .is_err()
    );
    let (source, captured) = &q.s.inputs[0];
    let plan = plan_captured(
        &q.s.source.fs,
        source,
        captured,
        &q.s.runtime.service,
        1024,
        InputLimits {
            max_captured_bytes: MAX_CAPTURED_BYTES as u64,
            max_hash_bytes: 14 * MAX_CAPTURED_BYTES as u64,
            deadline,
        },
    )
    .unwrap();
    let mut b = QuoteBuilder::new(&q.s.runtime, deadline).unwrap();
    b.push(&source.source_id, &plan.generation, &q.s.runtime, deadline)
        .unwrap();
    assert!(
        b.push(&source.source_id, &plan.generation, &q.s.runtime, deadline)
            .is_err()
    );
    assert!(
        b.finish(&q.s.runtime, Blake3Hash::digest([]), deadline)
            .is_err()
    );
    let mut b = QuoteBuilder::new(&q.s.runtime, deadline).unwrap();
    b.push(&source.source_id, &plan.generation, &q.s.runtime, deadline)
        .unwrap();
    let text = std::fs::read_to_string(&q.path)
        .unwrap()
        .replace("max_output_tokens=4096", "max_output_tokens=2048");
    quote_fixture::private(&q.path, text);
    q.s.runtime.service = ProviderConfig::load(&q.path)
        .unwrap()
        .authorize(
            &q.s.source.fs,
            &q.s.source.vault,
            "primary",
            crate::jobs::Capability::Generate,
        )
        .unwrap();
    assert!(
        b.finish(&q.s.runtime, Blake3Hash::digest([]), deadline)
            .is_err()
    );
    untouched(&q);
}

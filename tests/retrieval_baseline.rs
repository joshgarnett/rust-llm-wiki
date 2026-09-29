//! Fixed held-out corpus measurements, not a semantic-model benchmark.
use lwiki::{
    app::{OfflineApp, OperationOptions},
    catalog::{Catalog, SnapshotVerification},
    domain::*,
    graph::*,
    retrieval::{render, spaces::*, vectors::VectorStore, *},
    vault::{VaultFs, VaultRoot, WriterPermit},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

const REVISION: &str = "p21-fixed-v1";
// Fixed sorted byte corpus; changes require a new declared corpus revision.
const CORPUS_HASH: &str = "blake3:76e9b46977321f65a547019d062e864e731dbbe06bc1be6bba6995eb410c9e1b";
const FORWARD: &str = "assertion_00000000-0000-7000-8000-00000000000a";
const STALE: &str = "assertion_00000000-0000-7000-8000-00000000000f";
const SOURCE: &str = "source_00000000-0000-7000-8000-000000000005";
const MIRROR: &str = "source_00000000-0000-7000-8000-000000000006";
const METHODS: [&str; 6] = [
    "literal",
    "lexical",
    "exact_dense",
    "entity",
    "relationship",
    "hybrid",
];
const K: usize = 10;
const MAX_BYTES: usize = 6000;
const MAX_TOKENS: usize = 1000;

#[derive(Clone, Deserialize, Serialize)]
struct Question {
    id: String,
    query_class: String,
    text: String,
    documents: Vec<String>,
    graph: Vec<String>,
    evidence_set: Vec<String>,
}
fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/p21")
}
fn questions() -> Vec<Question> {
    serde_json::from_str(include_str!("fixtures/p21/queries.json")).unwrap()
}
fn files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, at: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, out)
            } else {
                out.insert(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .replace('\\', "/"),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}
fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for (name, bytes) in files(from) {
        let path = to.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
}
fn corpus_manifest() -> Value {
    let entries = files(&fixture_path().join("vault"));
    let mut hasher = blake3::Hasher::new();
    for (path, bytes) in &entries {
        hasher.update(&(path.len() as u64).to_le_bytes());
        hasher.update(path.as_bytes());
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }
    json!({"revision": REVISION, "blake3": format!("blake3:{}",hasher.finalize().to_hex()),
        "hash_encoding":"sorted relative path UTF-8 length u64le, path, bytes length u64le, bytes",
        "files": entries.iter().map(|(p,b)|json!({"path":p,"bytes":b.len(),"blake3":Blake3Hash::digest(b)})).collect::<Vec<_>>(),
        "queries_blake3":Blake3Hash::digest(include_bytes!("fixtures/p21/queries.json"))})
}
fn app(root: &Path) -> OfflineApp {
    OfflineApp::new(
        VaultFs::new(VaultRoot::explicit(root).unwrap()),
        OperationOptions {
            offline: true,
            ..Default::default()
        },
    )
    .unwrap()
}
fn spec() -> SpaceSpec {
    SpaceSpec {
        version: 1,
        endpoint_fingerprint: Blake3Hash::digest("p21-synthetic-no-endpoint"),
        profile_id: "synthetic-fixture".into(),
        service_id: "no-service".into(),
        model: "synthetic-hash-coordinates".into(),
        revision: Some("v1".into()),
        dimensions: Some(4),
        metric: "cosine".into(),
        normalization: "float64-l2-to-f32-le-v1".into(),
        render_version: RENDER_VERSION.into(),
        settings: EmbeddingSettings::default(),
    }
}
fn synthetic(input: &str) -> Vec<f32> {
    // Independent of questions/relevance labels; no semantic similarity is promised.
    blake3::hash(input.as_bytes()).as_bytes()[..4]
        .iter()
        .map(|n| (f32::from(*n) - 127.0) / 128.0)
        .collect()
}
fn seed(app: &OfflineApp) {
    let catalog = Catalog::new(app.fs().clone(), app.vault_id().clone());
    let writer = WriterPermit::acquire(app.fs().root(), Duration::from_secs(1)).unwrap();
    let reader = catalog.verified_snapshot(Some(&writer)).unwrap();
    let spec = spec();
    let units = render::corpus(&reader, &spec.settings).unwrap();
    let mut store = VectorStore::open(app.fs(), Some(&writer)).unwrap();
    let space = store.prepare_space(&spec).unwrap();
    for unit in &units {
        store
            .put_batch(
                &space,
                std::slice::from_ref(&unit.input_hash),
                &[synthetic(&unit.utf8)],
                true,
                &unit.dependency_fingerprint,
            )
            .unwrap();
    }
    store
        .memberships(&space, reader.snapshot(), &units, true)
        .unwrap();
    for q in questions() {
        store
            .put_batch(
                &space,
                &[spec.query(&q.text).unwrap().input_hash],
                &[synthetic(&q.text)],
                false,
                &Blake3Hash::digest([]),
            )
            .unwrap();
    }
    let mut other = spec.clone();
    other.model = "same-dimension-other-model".into();
    let other_space = store.prepare_space(&other).unwrap();
    assert_ne!(space, other_space);
    assert!(
        store
            .vector(&other_space, &units[0].input_hash)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .vector(
                &other_space,
                &spec.query(&questions()[0].text).unwrap().input_hash
            )
            .unwrap()
            .is_none()
    );
    let original = store.vector(&space, &units[0].input_hash).unwrap().unwrap();
    store
        .put_batch(
            &other_space,
            std::slice::from_ref(&units[0].input_hash),
            &[vec![0.0, 0.0, 0.0, 1.0]],
            true,
            &units[0].dependency_fingerprint,
        )
        .unwrap();
    store
        .memberships(&other_space, reader.snapshot(), &units[..1], false)
        .unwrap();
    assert_eq!(
        store
            .vector(&other_space, &units[0].input_hash)
            .unwrap()
            .unwrap(),
        vec![0.0, 0.0, 0.0, 1.0]
    );
    assert_eq!(
        store.vector(&space, &units[0].input_hash).unwrap().unwrap(),
        original
    );
    assert_eq!(
        store.coverage(&other_space, &units).unwrap().missing_units,
        units.len() - 1
    );
    assert_eq!(store.active().unwrap().unwrap().id, space);
}
fn request(method: &str) -> ContextRequest {
    let mut r = ContextRequest {
        budget: ContextBudget {
            max_bytes: MAX_BYTES,
            max_tokens: MAX_TOKENS,
            ..Default::default()
        },
        documents: QueryPlan {
            limits: SearchLimits {
                hits: K,
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    };
    match method {
        "literal" => r.documents.mode = SearchMode::Literal,
        "lexical" => r.documents.mode = SearchMode::Lexical,
        "exact_dense" => r.documents.mode = SearchMode::Semantic,
        "entity" | "relationship" => {
            r.target = ContextTarget::Graph;
            r.graph = Some(GraphPlan {
                strategy: if method == "entity" {
                    GraphStrategy::Entity
                } else {
                    GraphStrategy::Relationship
                },
                limits: GraphLimits {
                    hits: K,
                    depth: 2,
                    ..Default::default()
                },
                ..Default::default()
            });
        }
        "hybrid" => {
            r.target = ContextTarget::Combined;
            r.documents.mode = SearchMode::Hybrid;
            r.graph = Some(GraphPlan {
                strategy: GraphStrategy::Combined,
                seed_mode: GraphSeedMode::Semantic,
                limits: GraphLimits {
                    hits: K,
                    depth: 2,
                    ..Default::default()
                },
                ..Default::default()
            });
        }
        _ => panic!("unknown method"),
    }
    r
}
fn execute(app: &OfflineApp, q: &str, method: &str) -> ContextResult {
    let r = request(method);
    if matches!(method, "exact_dense" | "hybrid") {
        app.semantic_context(q, &r, None, false, false).unwrap()
    } else {
        let catalog = Catalog::new(app.fs().clone(), app.vault_id().clone());
        let writer = WriterPermit::acquire(app.fs().root(), Duration::from_secs(1)).unwrap();
        verification::context(&catalog, Some(&writer), q, &r).unwrap()
    }
}
fn citation_fields(c: &CitationRef) -> (&RecordId, &RecordId, ByteSpan, &Blake3Hash) {
    match c {
        CitationRef::Source(c) => (&c.source_id, &c.source_revision, c.span, &c.quote_hash),
        CitationRef::Assertion(c) => (&c.source_id, &c.source_revision, c.span, &c.quote_hash),
    }
}
fn verify(root: &Path, c: &ContextResult) -> (usize, usize) {
    assert!(!c.network_used);
    assert!(matches!(
        c.verification(),
        SnapshotVerification::VerifiedSnapshot { .. }
    ));
    assert_eq!(c.usage().rendered_bytes, c.text().len());
    assert_eq!(c.usage().estimated_tokens, c.text().len().div_ceil(4));
    assert!(c.usage().rendered_bytes <= MAX_BYTES);
    assert!(c.usage().estimated_tokens <= MAX_TOKENS);
    let mut citations = 0;
    for p in c.passages() {
        assert_eq!(p.eligibility, Eligibility::Current);
        let raw = fs::read(root.join(p.locator.path.as_str())).unwrap();
        assert_eq!(Blake3Hash::digest(&raw), p.locator.observed_hash);
        assert_eq!(
            p.span.slice(std::str::from_utf8(&raw).unwrap()).unwrap(),
            p.text
        );
        for citation in &p.citations {
            citations += 1;
            let (source, revision, span, hash) = citation_fields(citation);
            assert!(
                !revision.as_str().ends_with("000000000007"),
                "stale revision leaked"
            );
            if let CitationRef::Assertion(e) = citation {
                assert!(
                    !e.evidence_id.as_str().ends_with("000000000019"),
                    "stale evidence leaked"
                );
                assert_ne!(e.assertion_id.as_str(), STALE);
            }
            let source_note = lwiki::records::parse_note(
                &fs::read(root.join(format!("sources/{source}/source.md"))).unwrap(),
            );
            let source_record = source_note.canonical.as_ref().unwrap();
            assert_eq!(
                source_record.string("wiki_current_revision"),
                Some(revision.as_str())
            );
            assert_eq!(source_record.string("wiki_status"), Some("active"));
            let bytes =
                fs::read(root.join(format!("sources/{source}/revisions/{revision}/content.md")))
                    .unwrap();
            let quote = span.slice(std::str::from_utf8(&bytes).unwrap()).unwrap();
            assert_eq!(Blake3Hash::digest(quote.as_bytes()), *hash);
        }
    }
    for b in c.bundles() {
        assert_eq!(b.eligibility, Eligibility::Current);
        assert_ne!(b.assertion.record_id.as_str(), STALE);
        assert!(
            b.passage_indices.iter().any(|i| c.passages()[*i]
                .contributors
                .iter()
                .any(|e| e.stance == lwiki::sources::EvidenceStance::Supports
                    && e.reference.assertion_id == b.assertion.record_id)),
            "bundle lacks current support"
        );
        // Every bundle is an authored canonical assertion; traversal cannot invent an edge.
        let canonical = files(&root.join("knowledge/assertions"))
            .values()
            .find_map(|bytes| {
                lwiki::records::parse_note(bytes)
                    .canonical
                    .filter(|r| r.id() == &b.assertion.record_id)
            })
            .unwrap();
        assert_eq!(
            canonical.string("wiki_subject_id"),
            Some(b.subject.record_id.as_str())
        );
        assert_eq!(
            canonical.string("wiki_predicate"),
            Some(b.predicate.as_str())
        );
        match &b.object {
            GraphObject::Entity { record_ref } => assert_eq!(
                canonical.string("wiki_object_id"),
                Some(record_ref.record_id.as_str())
            ),
            GraphObject::Literal {
                literal_type,
                value,
            } => {
                assert_eq!(
                    canonical.string("wiki_literal_type"),
                    Some(literal_type.as_str())
                );
                assert_eq!(canonical.string("wiki_literal_value"), Some(value.as_str()));
            }
        }
        assert_eq!(
            canonical
                .field("wiki_negated")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            b.qualifiers.negated
        );
        assert_eq!(
            canonical.string("wiki_modality").unwrap_or("asserted"),
            b.qualifiers.modality
        );
        assert_eq!(
            canonical.string("wiki_valid_from"),
            b.qualifiers.valid_from.as_deref()
        );
        assert_eq!(
            canonical.string("wiki_valid_until"),
            b.qualifiers.valid_until.as_deref()
        );
        assert_eq!(
            canonical.string("wiki_property"),
            b.qualifiers.property.as_deref()
        );
        assert_eq!(canonical.string("wiki_unit"), b.qualifiers.unit.as_deref());
        for endpoint in std::iter::once(&b.subject).chain(match &b.object {
            GraphObject::Entity { record_ref } => Some(record_ref),
            _ => None,
        }) {
            assert_eq!(endpoint.expected_kind, RecordKind::Entity);
            assert_eq!(endpoint.vault_id, b.assertion.vault_id);
        }
    }
    (citations, c.bundles().len())
}
fn keys(c: &ContextResult, method: &str) -> Vec<String> {
    let mut keys = Vec::new();
    if !matches!(method, "entity" | "relationship") {
        keys.extend(
            c.passages()
                .iter()
                .map(|p| p.locator.path.as_str().to_owned()),
        );
    }
    if matches!(method, "entity" | "relationship" | "hybrid") {
        keys.extend(
            c.bundles()
                .iter()
                .map(|b| format!("assertion:{}", b.assertion.record_id)),
        );
    }
    let mut seen = BTreeSet::new();
    keys.retain(|k| seen.insert(k.clone()));
    keys.truncate(K);
    keys
}
fn quality(q: &Question, c: &ContextResult, method: &str, root: &Path) -> Value {
    let (citations, supported) = verify(root, c);
    let ranked = keys(c, method);
    let relevant: BTreeSet<_> = match method {
        "entity" | "relationship" => q.graph.iter().cloned().collect(),
        "hybrid" => q.documents.iter().chain(&q.graph).cloned().collect(),
        _ => q.documents.iter().cloned().collect(),
    };
    let positives = ranked.iter().filter(|k| relevant.contains(*k)).count();
    let dcg = ranked
        .iter()
        .enumerate()
        .filter(|(_, k)| relevant.contains(*k))
        .map(|(i, _)| 1.0 / ((i + 2) as f64).log2())
        .sum::<f64>();
    let idcg = (0..relevant.len().min(K))
        .map(|i| 1.0 / ((i + 2) as f64).log2())
        .sum::<f64>();
    let evidence_notes = files(&root.join("knowledge/evidence"));
    let complete_evidence = q.evidence_set.iter().all(|required| {
        let note = evidence_notes
            .values()
            .find_map(|bytes| {
                lwiki::records::parse_note(bytes)
                    .canonical
                    .filter(|r| r.id().as_str() == required)
            })
            .unwrap();
        c.passages()
            .iter()
            .flat_map(|p| &p.citations)
            .any(|cit| match cit {
                CitationRef::Assertion(e) => e.evidence_id.as_str() == required,
                CitationRef::Source(e) => {
                    note.string("wiki_source_id") == Some(e.source_id.as_str())
                        && note.string("wiki_source_revision") == Some(e.source_revision.as_str())
                        && note
                            .field("wiki_span_start")
                            .and_then(Value::as_u64)
                            .is_some_and(|s| e.span.start() <= s)
                        && note
                            .field("wiki_span_end")
                            .and_then(Value::as_u64)
                            .is_some_and(|end| e.span.end() >= end)
                }
            })
    });
    json!({"ranked_keys":ranked, "relevant_count":relevant.len(), "recall_at_k":(!relevant.is_empty()).then(|| positives as f64/relevant.len() as f64),
        "ndcg_at_k":(idcg>0.0).then(||dcg/idcg), "complete_evidence_set_recall":(!q.evidence_set.is_empty()).then(||usize::from(complete_evidence)),
        "citation_count":citations,"citation_validity":(citations>0).then_some(1.0),"citation_resolution_errors":0,
        "supported_assertion_count":supported,"supported_assertion_precision":(supported>0).then_some(1.0),
        "rendered_bytes":c.usage().rendered_bytes,"estimated_tokens":c.usage().estimated_tokens,"omissions":c.omissions(),
        "truncated":c.truncated(),"network_used":c.network_used,"warnings":c.warnings()})
}
#[cfg(unix)]
fn peak_rss_bytes() -> Option<u64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage writes the complete rusage output to a valid aligned allocation.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: successful getrusage initialized the allocation.
    let max = unsafe { usage.assume_init() }.ru_maxrss;
    let value = u64::try_from(max).ok()?;
    Some(if cfg!(target_os = "macos") {
        value
    } else {
        value * 1024
    })
}
#[cfg(not(unix))]
fn peak_rss_bytes() -> Option<u64> {
    None
}
fn disk(root: &Path) -> Value {
    let entries = files(root);
    json!({"logical_bytes":entries.values().map(Vec::len).sum::<usize>(),"file_count":entries.len(),
        "cache_logical_bytes":entries.iter().filter(|(p,_)|p.starts_with(".wiki/cache/")).map(|(_,b)|b.len()).sum::<usize>()})
}
fn system_command(program: &str, args: &[&str]) -> Option<String> {
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
}
#[test]
#[ignore = "explicit isolated measurement helper spawned by heldout_fixed_corpus_equal_budget_baseline"]
fn p21_measurement_child() {
    let root = PathBuf::from(
        std::env::var_os("LWIKI_P21_CHILD_VAULT").expect("explicit disposable vault"),
    );
    let output = PathBuf::from(
        std::env::var_os("LWIKI_P21_CHILD_OUTPUT").expect("explicit measurement output"),
    );
    let method = std::env::var("LWIKI_P21_CHILD_METHOD").unwrap();
    let index: usize = std::env::var("LWIKI_P21_CHILD_QUESTION")
        .unwrap()
        .parse()
        .unwrap();
    let q = &questions()[index];
    let before = disk(&root);
    let start = Instant::now();
    let app = app(&root);
    let cold = execute(&app, &q.text, &method);
    let cold_ns = start.elapsed().as_nanos();
    let start = Instant::now();
    let warm = execute(&app, &q.text, &method);
    let warm_ns = start.elapsed().as_nanos();
    assert_eq!(cold.text(), warm.text());
    assert_eq!(cold.passages(), warm.passages());
    assert_eq!(cold.bundles(), warm.bundles());
    let measured = quality(q, &warm, &method, &root);
    let result = json!({"question":q,"method":method,"cold_ns":cold_ns.to_string(),"warm_ns":warm_ns.to_string(),"quality":measured,
        "process_peak_rss_bytes":peak_rss_bytes(),"disk_before":before,"disk_after":disk(&root),
        "provider_usage":{"requests":0,"input_tokens":0,"output_tokens":0,"charged_nanos":0,"basis":"cached synthetic vectors; no provider runtime/dispatcher constructed"}});
    fs::write(output, serde_json::to_vec_pretty(&result).unwrap()).unwrap();
}
#[test]
fn heldout_fixed_corpus_equal_budget_baseline() {
    let manifest = corpus_manifest();
    assert_eq!(
        manifest["blake3"].as_str().unwrap(),
        CORPUS_HASH,
        "pin the observed corpus before qualification"
    );
    let base = tempfile::tempdir().unwrap();
    let vault = base.path().join("seeded-vault");
    copy(&fixture_path().join("vault"), &vault);
    seed(&app(&vault));
    let mut measurements = Vec::new();
    for method in METHODS {
        for (index, _) in questions().iter().enumerate() {
            let child_vault = base.path().join(format!("{method}-{index}"));
            copy(&vault, &child_vault);
            let output = base.path().join(format!("{method}-{index}.json"));
            let child = Command::new(std::env::current_exe().unwrap())
                .env_clear()
                .args([
                    "--exact",
                    "p21_measurement_child",
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env("LWIKI_P21_CHILD_VAULT", &child_vault)
                .env("LWIKI_P21_CHILD_OUTPUT", &output)
                .env("LWIKI_P21_CHILD_METHOD", method)
                .env("LWIKI_P21_CHILD_QUESTION", index.to_string())
                .output()
                .unwrap();
            assert!(
                child.status.success(),
                "{method}/{index}: {} {}",
                String::from_utf8_lossy(&child.stdout),
                String::from_utf8_lossy(&child.stderr)
            );
            measurements.push(serde_json::from_slice::<Value>(&fs::read(output).unwrap()).unwrap());
        }
    }
    let mut by_class = BTreeMap::<String, Vec<&Value>>::new();
    for m in &measurements {
        by_class
            .entry(format!(
                "{}/{}",
                m["method"].as_str().unwrap(),
                m["question"]["query_class"].as_str().unwrap()
            ))
            .or_default()
            .push(m);
    }
    let means: BTreeMap<_, _> = by_class.iter().map(|(key, values)| {
        let mut metrics = serde_json::Map::new();
        for metric in ["recall_at_k","ndcg_at_k","complete_evidence_set_recall","citation_validity","supported_assertion_precision"] {
            let ns: Vec<_> = values.iter().filter_map(|v|v["quality"][metric].as_f64()).collect();
            metrics.insert(metric.into(),json!({"applicable_questions":ns.len(),"mean":(!ns.is_empty()).then(||ns.iter().sum::<f64>()/ns.len() as f64)}));
        }
        (key.clone(), Value::Object(metrics))
    }).collect();
    let artifact = json!({"version":1,"corpus":manifest,"k":K,
        "measurement_build":{"debug_assertions":cfg!(debug_assertions),"test_executable_blake3":Blake3Hash::digest(fs::read(std::env::current_exe().unwrap()).unwrap()),"rustc":system_command("rustc", &["--version"]),"cargo_lock_blake3":Blake3Hash::digest(fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.lock")).unwrap())},"context_budget":{"max_bytes":MAX_BYTES,"max_estimated_tokens":MAX_TOKENS,"accounting":"ceil(rendered_utf8_bytes/4)"},
        "hardware":{"os_arch":format!("{} {}",std::env::consts::OS,std::env::consts::ARCH),"uname":system_command("uname", &["-a"]),
            "model":system_command("sysctl", &["-n","hw.model"]),"cpu":system_command("sysctl", &["-n","machdep.cpu.brand_string"]),"physical_memory_bytes":system_command("sysctl", &["-n","hw.memsize"])},
        "limits":{"dense":"deterministic synthetic hash vectors; no semantic-model quality or ranking-gain claim","latency":"fresh process with prebuilt on-disk caches; OS caches not flushed; warm repeats same query/process", "memory":"per-child process peak RSS, includes setup/retrieval/validation","disk":"logical file bytes including transient SQLite files","quality":"target-specific binary relevance; exported passage paths followed by bundle IDs in rendered output order, deduplicated k=10; held-out labels, no tuning; no generated answer precision"},
        "by_method_and_class":means,"measurements":measurements});
    let path = std::env::var_os("LWIKI_P21_EVIDENCE")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("lwiki-p21-retrieval-baseline.json"));
    fs::write(&path, serde_json::to_vec_pretty(&artifact).unwrap()).unwrap();
    println!("P21 measured artifact: {}", path.display());
}
#[test]
fn fixed_corpus_current_evidence_withdrawal_homonyms_and_isolated_spaces() {
    let temp = tempfile::tempdir().unwrap();
    copy(&fixture_path().join("vault"), temp.path());
    let app = app(temp.path());
    seed(&app);
    let catalog = Catalog::new(app.fs().clone(), app.vault_id().clone());
    let writer = WriterPermit::acquire(app.fs().root(), Duration::from_secs(1)).unwrap();
    let reader = catalog.verified_snapshot(Some(&writer)).unwrap();
    let graph = lwiki::graph::query::query(
        &reader,
        "Alex Kim",
        &GraphPlan {
            strategy: GraphStrategy::Entity,
            ..Default::default()
        },
    )
    .unwrap();
    let homonyms: BTreeSet<_> = graph
        .seeds
        .iter()
        .filter(|s| s.title == "Alex Kim")
        .map(|s| s.record_ref.record_id.as_str())
        .collect();
    assert_eq!(
        homonyms,
        BTreeSet::from([
            "entity_00000000-0000-7000-8000-000000000001",
            "entity_00000000-0000-7000-8000-000000000002"
        ])
    );
    drop(reader);
    drop(writer);
    for method in METHODS {
        verify(temp.path(), &execute(&app, "uses", method));
    }
    app.source_withdraw(
        RecordId::new(MIRROR).unwrap(),
        "P21 remove one independent support",
    )
    .unwrap();
    let remaining = execute(&app, FORWARD, "relationship");
    assert_eq!(remaining.bundles().len(), 1);
    verify(temp.path(), &remaining);
    assert!(
        remaining
            .passages()
            .iter()
            .flat_map(|p| &p.citations)
            .all(|c| citation_fields(c).0.as_str() != MIRROR)
    );
    app.source_withdraw(
        RecordId::new(SOURCE).unwrap(),
        "P21 remove last independent support",
    )
    .unwrap();
    for method in METHODS {
        let context = execute(&app, "uses", method);
        verify(temp.path(), &context);
        assert!(
            context.bundles().is_empty(),
            "withdrawn assertion leaked in {method}"
        );
        assert!(
            context
                .passages()
                .iter()
                .flat_map(|p| &p.citations)
                .all(|c| ![SOURCE, MIRROR].contains(&citation_fields(c).0.as_str())),
            "withdrawn citation leaked in {method}"
        );
    }
    assert!(execute(&app, FORWARD, "relationship").bundles().is_empty());
}

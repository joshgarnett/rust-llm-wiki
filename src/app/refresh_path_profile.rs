//! Explicit disposable-clone attribution, not a CLI latency or scaling gate.
use crate::{
    app::{OfflineApp, OperationOptions},
    catalog::{
        Catalog,
        query_types::{QueryCatalog, QueryReadLimits},
    },
    domain::{Blake3Hash, CitationRef, RecordId},
    retrieval::{ContextRequest, ContextScope, verification},
    sources::{CaptureRequest, ExtractionInput, SourceOrigin},
    vault::{VaultFs, VaultRoot, paths::profile},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    env,
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    time::Instant,
};

std::thread_local! {
    static PHASES: RefCell<Option<BTreeMap<&'static str, u128>>> = const { RefCell::new(None) };
}
pub(crate) struct PhaseGuard {
    name: &'static str,
    start: Option<Instant>,
}
pub(crate) fn phase(name: &'static str) -> PhaseGuard {
    PhaseGuard {
        name,
        start: PHASES
            .with(|value| value.borrow().is_some())
            .then(Instant::now),
    }
}
impl Drop for PhaseGuard {
    fn drop(&mut self) {
        if let Some(start) = self.start {
            PHASES.with(|value| {
                if let Some(phases) = value.borrow_mut().as_mut() {
                    *phases.entry(self.name).or_default() += start.elapsed().as_nanos();
                }
            });
        }
    }
}
type DiagnosticResult<T> = Result<T, Box<dyn std::error::Error>>;
fn invalid(message: &str) -> Box<dyn std::error::Error> {
    io::Error::new(io::ErrorKind::InvalidInput, message).into()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CloneMarker {
    version: u32,
    kind: String,
    original_export: PathBuf,
    corpus_blake3: Blake3Hash,
}
struct Fixture {
    container: PathBuf,
    vault: PathBuf,
    report: PathBuf,
    marker: CloneMarker,
    corpus_hash: Blake3Hash,
    count: usize,
    source: RecordId,
    revision: RecordId,
    title: String,
    search_marker: String,
    body: Vec<u8>,
    inventory_files: usize,
    inventory_bytes: u64,
}
fn absolute(path: &Path) -> DiagnosticResult<()> {
    if !path.is_absolute()
        || path.to_str().is_none()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(invalid(
            "profile paths must be normalized absolute UTF8 paths",
        ));
    }
    Ok(())
}
fn ancestors(path: &Path) -> DiagnosticResult<()> {
    absolute(path)?;
    let mut current = PathBuf::new();
    for part in path.components() {
        current.push(part);
        let metadata = fs::symlink_metadata(&current)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(invalid("profile directory ancestor is unsafe"));
        }
    }
    Ok(())
}
fn regular(path: &Path) -> DiagnosticResult<u64> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(invalid("profile accepts regular files only"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(invalid("hardlinked profile file refuses"));
        }
    }
    Ok(metadata.len())
}
fn bounded(path: &Path, limit: u64) -> DiagnosticResult<Vec<u8>> {
    if regular(path)? > limit {
        return Err(invalid("profile input exceeds ceiling"));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid("profile input grew beyond ceiling"));
    }
    Ok(bytes)
}
fn hash_file(path: &Path) -> DiagnosticResult<Blake3Hash> {
    regular(path)?;
    let mut hash = blake3::Hasher::new();
    let mut file = fs::File::open(path)?;
    let mut bytes = [0; 65536];
    loop {
        let count = file.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        hash.update(&bytes[..count]);
    }
    Ok(Blake3Hash::new(format!(
        "blake3:{}",
        hash.finalize().to_hex()
    ))?)
}
fn tree(path: &Path, files: &mut usize, bytes: &mut u64) -> DiagnosticResult<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() {
            return Err(invalid("symlink in profile clone"));
        }
        if metadata.is_dir() {
            tree(&entry.path(), files, bytes)?;
        } else {
            *bytes = bytes
                .checked_add(regular(&entry.path())?)
                .ok_or_else(|| invalid("clone byte overflow"))?;
            *files += 1;
            if *files > 100000 || *bytes > 20 * 1024 * 1024 * 1024 {
                return Err(invalid("profile clone inventory exceeds finite ceiling"));
            }
        }
    }
    Ok(())
}
fn field<'a>(value: &'a Value, key: &str) -> DiagnosticResult<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("missing profile manifest string"))
}
impl Fixture {
    fn from_env() -> DiagnosticResult<Self> {
        let container = env::var_os("LWIKI_REFRESH_PROFILE_CLONE")
            .map(PathBuf::from)
            .ok_or_else(|| invalid("LWIKI_REFRESH_PROFILE_CLONE required"))?;
        let report = env::var_os("LWIKI_REFRESH_PROFILE_REPORT")
            .map(PathBuf::from)
            .ok_or_else(|| invalid("LWIKI_REFRESH_PROFILE_REPORT required"))?;
        ancestors(&container)?;
        absolute(&report)?;
        ancestors(
            report
                .parent()
                .ok_or_else(|| invalid("report has no parent"))?,
        )?;
        if fs::symlink_metadata(&report).is_ok() {
            return Err(invalid("profile report already exists"));
        }
        let marker: CloneMarker = serde_json::from_slice(&bounded(
            &container.join("refresh-profile-clone.json"),
            4096,
        )?)?;
        absolute(&marker.original_export)?;
        if marker.version != 1
            || marker.kind != "disposable-refresh-profile-clone"
            || container.starts_with(&marker.original_export)
            || marker.original_export.starts_with(&container)
            || report.starts_with(&marker.original_export)
        {
            return Err(invalid(
                "profile requires a disjoint explicitly marked disposable clone",
            ));
        }
        let vault = container.join("vault");
        ancestors(&vault)?;
        if report.starts_with(&vault) {
            return Err(invalid("profile report must be outside vault"));
        }
        let corpus_bytes = bounded(&container.join("corpus.json"), 64 * 1024 * 1024)?;
        let corpus_hash = Blake3Hash::digest(&corpus_bytes);
        if corpus_hash != marker.corpus_blake3 {
            return Err(invalid("clone corpus pin differs"));
        }
        let corpus: Value = serde_json::from_slice(&corpus_bytes)?;
        let count = corpus["source_count"]
            .as_u64()
            .ok_or_else(|| invalid("source count missing"))? as usize;
        if !matches!(count, 2 | 1000 | 10000)
            || corpus["version"] != 1
            || corpus["kind"] != "normalized-refresh-fixture"
            || corpus["proof_layout_version"] != 2
            || corpus["revision_ownership_version"] != 1
            || corpus["target_prior_extra_revisions"] != 0
            || corpus["sources"]
                .as_array()
                .is_none_or(|values| values.len() != count)
        {
            return Err(invalid(
                "profile requires the accepted2/1k/10k zero-history normalized fixture",
            ));
        }
        let source = &corpus["sources"][0];
        let id = RecordId::new(field(source, "source_id")?)?;
        let revision = RecordId::new(field(source, "revision_id")?)?;
        if source["index"] != 0 || corpus["target_current_revision"] != revision.as_str() {
            return Err(invalid("profile target identity differs"));
        }
        let body_path = vault.join(format!("sources/{id}/revisions/{revision}/content.md"));
        let body = bounded(&body_path, 1024 * 1024)?;
        if body.len() as u64
            != source["bytes"]
                .as_u64()
                .ok_or_else(|| invalid("source bytes missing"))?
            || Blake3Hash::digest(&body).as_str() != field(source, "blake3")?
            || !(1024..=1024 * 1024).contains(&body.len())
        {
            return Err(invalid("profile target body differs from manifest"));
        }
        std::str::from_utf8(&body)?;
        let title = field(source, "title")?.to_owned();
        let search_marker = field(source, "marker")?.to_owned();
        if title != "Synthetic capture 000000"
            || search_marker != "refreshprobe000000v000000"
            || std::str::from_utf8(&body)?.matches(&search_marker).count() != 1
        {
            return Err(invalid(
                "profile target is not the expected synthetic capture",
            ));
        }
        let (mut inventory_files, mut inventory_bytes) = (0, 0);
        tree(&vault, &mut inventory_files, &mut inventory_bytes)?;
        // Preflight setup is intentionally outside every operation interval.
        let entries = corpus["files"]
            .as_array()
            .ok_or_else(|| invalid("fixture inventory missing"))?;
        if entries.len() != inventory_files {
            return Err(invalid("clone has unlisted files"));
        }
        let mut previous: Option<&str> = None;
        for row in entries {
            let relative = field(row, "path")?;
            crate::domain::VaultRelativePath::new(relative)?;
            if previous.is_some_and(|old| old >= relative) {
                return Err(invalid("unsorted/duplicate clone inventory"));
            }
            previous = Some(relative);
            let path = vault.join(relative);
            if regular(&path)?
                != row["bytes"]
                    .as_u64()
                    .ok_or_else(|| invalid("inventory bytes missing"))?
                || hash_file(&path)?.as_str() != field(row, "blake3")?
            {
                return Err(invalid("closed clone inventory differs from seed pin"));
            }
        }
        Ok(Self {
            container,
            vault,
            report,
            marker,
            corpus_hash,
            count,
            source: id,
            revision,
            title,
            search_marker,
            body,
            inventory_files,
            inventory_bytes,
        })
    }
}

fn verify_context(
    catalog: &Catalog,
    source: &RecordId,
    revision: &RecordId,
    body: &str,
    marker: &str,
) -> DiagnosticResult<Value> {
    let mut request = ContextRequest {
        scope: ContextScope::IndexedEvidence,
        ..Default::default()
    };
    request.documents.filters.source_ids.push(source.clone());
    request.documents.limits.excerpt_bytes = 1024;
    let start = Instant::now();
    let result = verification::context(catalog, None, marker, &request)?;
    let elapsed_ns = start.elapsed().as_nanos();
    if result.network_used
        || result.passages().is_empty()
        || !result
            .passages()
            .iter()
            .any(|passage| passage.text.contains(marker))
    {
        return Err(invalid("profile indexed context missed current marker"));
    }
    for passage in result.passages() {
        if passage.text != passage.span.slice(body)? || passage.citations.len() != 1 {
            return Err(invalid(
                "profile context passage differs from selected body",
            ));
        }
        match &passage.citations[0] {
            CitationRef::Source(span)
                if &span.source_id == source
                    && &span.source_revision == revision
                    && span.span == passage.span
                    && span.quote_hash == Blake3Hash::digest(passage.text.as_bytes()) => {}
            _ => return Err(invalid("profile context citation binding differs")),
        }
    }
    Ok(json!({"elapsed_ns":elapsed_ns,"snapshot":result.snapshot(),
        "verification":result.verification(),"passages":result.passages().len(),"exact_citations":true}))
}

#[test]
#[ignore = "requires supervisor-owned explicit2/1k/10k disposable clone and resource ceilings"]
fn profile_normalized_source_refresh() {
    run().unwrap();
}
fn run() -> DiagnosticResult<()> {
    let setup_start = Instant::now();
    let fixture = Fixture::from_env()?;
    let fs = VaultFs::new(VaultRoot::explicit(&fixture.vault)?);
    let app = OfflineApp::new(
        fs.clone(),
        OperationOptions {
            offline: true,
            ..Default::default()
        },
    )?;
    let catalog = Catalog::new(fs, app.vault_id().clone());
    let setup_ns = setup_start.elapsed().as_nanos();
    let mut revision = fixture.revision.clone();
    let mut body = String::from_utf8(fixture.body.clone())?;
    let mut marker = fixture.search_marker.clone();
    let initial_context = verify_context(&catalog, &fixture.source, &revision, &body, &marker)?;
    let initial = catalog.query_snapshot(QueryReadLimits::default())?;
    let mut snapshot = initial.snapshot().clone();
    if snapshot.publication().is_none() {
        return Err(invalid("profile clone is not normalized"));
    }
    drop(initial);
    let mut cases = Vec::new();
    for name in ["noop", "title-only", "changed"] {
        let changed_marker = "refreshprobe000000v000001";
        let next_body = if name == "changed" {
            body.replacen(&marker, changed_marker, 1)
        } else {
            body.clone()
        };
        let request = CaptureRequest {
            title: fixture.title.clone(),
            origin_kind: SourceOrigin::LocalFile,
            origin: "fixture-000000.md".into(),
            original: next_body.as_bytes().to_vec(),
            extraction: ExtractionInput::Utf8Preserve,
            media_type: None,
        };
        PHASES.with(|value| *value.borrow_mut() = Some(BTreeMap::new()));
        profile::begin();
        let utc_start_unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis();
        let start = Instant::now();
        let outcome = app.source_refresh_with_title(
            fixture.source.clone(),
            request,
            (name == "title-only").then_some("Synthetic capture 000000 profile title"),
        );
        let elapsed_ns = start.elapsed().as_nanos();
        let utc_end_unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis();
        let paths = profile::finish();
        let phases = PHASES.with(|value| value.borrow_mut().take().unwrap());
        let value = match outcome {
            Ok(outcome) => (|| -> DiagnosticResult<Value> {
                let reader = catalog.query_snapshot(QueryReadLimits::default())?;
                let next_snapshot = reader.snapshot().clone();
                let source_record = reader
                    .record(&fixture.source)?
                    .ok_or_else(|| invalid("selected source missing"))?;
                let expected_title = if name == "noop" {
                    fixture.title.as_str()
                } else {
                    "Synthetic capture 000000 profile title"
                };
                if source_record.record.title() != expected_title {
                    return Err(invalid("profile source title differs"));
                }
                drop(reader);
                let expected_epoch = snapshot.generation + u64::from(name != "noop");
                let next_revision = outcome
                    .allocated_ids
                    .get("revision")
                    .ok_or_else(|| invalid("missing allocated revision"))?
                    .clone();
                if next_snapshot.generation != expected_epoch
                    || (name == "noop") != outcome.change.is_none()
                    || outcome.reused != (name != "changed")
                    || (name != "changed" && next_revision != revision)
                    || (name == "changed" && next_revision == revision)
                {
                    return Err(invalid("profile refresh outcome/epoch differs"));
                }
                if name != "noop" && outcome.status != Some(crate::changes::ChangeStatus::Committed)
                {
                    return Err(invalid("profile refresh was not committed"));
                }
                if name == "changed" {
                    marker = changed_marker.into();
                }
                body = next_body;
                revision = next_revision;
                let parent = fixture
                    .vault
                    .join(format!("sources/{}/revisions/{revision}", fixture.source));
                if bounded(&parent.join("content.md"), 1024 * 1024)? != body.as_bytes()
                    || bounded(&parent.join("original.bin"), 1024 * 1024)? != body.as_bytes()
                {
                    return Err(invalid(
                        "profile canonical assets differ from requested bytes",
                    ));
                }
                let context = verify_context(&catalog, &fixture.source, &revision, &body, &marker)?;
                if context["snapshot"] != serde_json::to_value(&next_snapshot)? {
                    return Err(invalid("profile context observed another epoch"));
                }
                snapshot = next_snapshot;
                Ok(json!({"ok":true,"outcome":outcome,"context":context}))
            })()
            .unwrap_or_else(|error| json!({"ok":false,"error":error.to_string()})),
            Err(error) => json!({"ok":false,"error":{"code":error.code,"message":error.message}}),
        };
        let failed = value["ok"] != true;
        cases.push(
            json!({"case":name,"refresh_elapsed_ns":elapsed_ns,"phases_ns":phases,
            "passed":!failed,"measurement_window":{"utc_start_unix_ms":utc_start_unix_ms,
                "utc_end_unix_ms":utc_end_unix_ms,"contains_driver_inventory_sweep":false},
            "paths":paths,"result":value}),
        );
        if failed {
            break;
        }
    }
    let complete = cases.len() == 3 && cases.iter().all(|case| case["result"]["ok"] == true);
    let report = json!({"version":1,"kind":"refresh-path-attribution","complete":complete,
        "status":if complete {"passed"} else {"failed"},
        "clone":fixture.container,"original_export":fixture.marker.original_export,
        "corpus_blake3":fixture.corpus_hash,"test_binary_blake3":hash_file(&env::current_exe()?)?,
        "source_count":fixture.count,"source_id":fixture.source,"setup_elapsed_ns":setup_ns,
        "preflight_inventory_files":fixture.inventory_files,"preflight_inventory_bytes":fixture.inventory_bytes,
        "preflight_inventory_complete_before_measurements":true,
        "initial_context":initial_context,"cases":cases,
        "limits":"supervisor owns wall/RSS/account/free-space ceilings",
        "counter_note":"opens counts read_dir attempts (including failed opens); entries counts yielded iterator items before decoding; folds counts sibling names actually folded, excluding query/planned-prefix folds. Marker root is unknown and classified other. Immutable walk elapsed is inclusive.",
        "timing_note":"Direct OfflineApp invocation excludes CLI parse/config overhead. apply includes publish. Portable elapsed contains enumeration subtimers, immutable recursive walks are inclusive: do not sum overlaps. Context is separate. UTC windows are external observations, not cross-process monotonic durations. Setup inventory sweeps end before all operation windows. Instrumentation changes timing; original CLI benchmarks remain authoritative."});
    let bytes = serde_json::to_vec_pretty(&report)?;
    if bytes.len() > 1024 * 1024 {
        return Err(invalid("profile report exceeds ceiling"));
    }
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&fixture.report)?;
    output.write_all(&bytes)?;
    output.write_all(b"\n")?;
    output.sync_all()?;
    println!(
        "{}",
        serde_json::to_string(
            &json!({"profile_report":fixture.report,"complete":complete,"source_count":fixture.count})
        )?
    );
    if !complete {
        return Err(invalid(
            "profile refresh/citation check failed; report retained",
        ));
    }
    Ok(())
}

#[test]
fn path_counters_count_real_entries_and_remain_thread_local() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("WIKI.md"),
        include_bytes!("../../tests/fixtures/bootstrap/vault/WIKI.md"),
    )
    .unwrap();
    fs::write(temp.path().join("noise.md"), "noise").unwrap();
    let root = VaultRoot::explicit(temp.path()).unwrap();
    profile::begin();
    root.validate_portable_paths(&[crate::domain::VaultRelativePath::new("new.md").unwrap()])
        .unwrap();
    std::thread::spawn(|| {
        profile::begin();
        assert_eq!(profile::finish().portable_calls, 0);
    })
    .join()
    .unwrap();
    let counts = profile::finish();
    assert_eq!(counts.portable_calls, 1);
    let sibling = &counts.enumerations["physical:vault_root"];
    assert_eq!((sibling.opens, sibling.entries, sibling.folds), (1, 2, 2));
    assert!(counts.portable_elapsed_ns >= sibling.elapsed_ns);
}

#[test]
fn driver_refuses_unsafe_clone_paths_and_links() {
    assert!(absolute(Path::new("relative")).is_err());
    assert!(absolute(Path::new("/tmp/profile/../original")).is_err());
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("file"), b"original").unwrap();
    assert!(ancestors(&temp.path().join("file")).is_err());
    #[cfg(unix)]
    {
        fs::hard_link(temp.path().join("file"), temp.path().join("linked")).unwrap();
        assert!(regular(&temp.path().join("linked")).is_err());
        std::os::unix::fs::symlink(temp.path(), temp.path().join("alias")).unwrap();
        assert!(ancestors(&temp.path().join("alias")).is_err());
    }
}

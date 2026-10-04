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

/// Separate fixture contract: the refresh-only envelope above is unchanged.
mod source_add_profile {
    use super::*;
    use crate::{
        app::MutationOutcome,
        changes::{ChangeStatus, RevisionOwnershipLookup, RevisionTreeKey},
        domain::VaultRelativePath,
        sources::{SourceCaptureState, SourceRefreshLookup},
    };
    use std::{collections::BTreeSet, time::SystemTime};

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Entry {
        path: String,
        bytes: u64,
        blake3: Blake3Hash,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct AddManifest {
        version: u32,
        kind: String,
        original_export: PathBuf,
        whole: PathBuf,
        staged: PathBuf,
        source_count: usize,
        change_count: usize,
        object_count: usize,
        input: PathBuf,
        input_blake3: Blake3Hash,
        title: String,
        origin: String,
        marker: String,
        files: Vec<Entry>,
        directories: Vec<String>,
    }
    type CanonicalPins = BTreeMap<String, (Blake3Hash, SystemTime)>;

    fn directory_names(
        root: &Path,
        current: &Path,
        out: &mut BTreeSet<String>,
    ) -> DiagnosticResult<()> {
        for entry in fs::read_dir(current)? {
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() {
                return Err(invalid("linked add fixture directory"));
            }
            if metadata.is_dir() {
                let name = entry
                    .path()
                    .strip_prefix(root)?
                    .to_str()
                    .ok_or_else(|| invalid("non-UTF8 add fixture path"))?
                    .to_owned();
                if out.len() >= 100000 {
                    return Err(invalid("add fixture directory ceiling"));
                }
                out.insert(name);
                directory_names(root, &entry.path(), out)?;
            }
        }
        Ok(())
    }
    fn count(root: &Path, relative: &str, directories: bool) -> DiagnosticResult<usize> {
        let mut count = 0;
        for entry in fs::read_dir(root.join(relative))? {
            let metadata = fs::symlink_metadata(entry?.path())?;
            if metadata.file_type().is_symlink() {
                return Err(invalid("linked add fixture member"));
            }
            if (directories && metadata.is_dir()) || (!directories && metadata.is_file()) {
                count += 1;
            }
        }
        Ok(count)
    }
    fn validate_clone(manifest: &AddManifest, root: &Path) -> DiagnosticResult<CanonicalPins> {
        ancestors(root)?;
        let (mut files, mut bytes) = (0, 0);
        tree(root, &mut files, &mut bytes)?;
        if files != manifest.files.len() {
            return Err(invalid("add clone has unlisted/missing files"));
        }
        let mut previous: Option<&str> = None;
        let mut pins = BTreeMap::new();
        for entry in &manifest.files {
            VaultRelativePath::new(&entry.path)?;
            if previous.is_some_and(|name| name >= entry.path.as_str()) {
                return Err(invalid("unsorted/duplicate add inventory"));
            }
            previous = Some(&entry.path);
            let path = root.join(&entry.path);
            if regular(&path)? != entry.bytes || hash_file(&path)? != entry.blake3 {
                return Err(invalid("add clone file differs from pin"));
            }
            if !entry.path.starts_with(".wiki/") && !entry.path.starts_with("changes/") {
                pins.insert(
                    entry.path.clone(),
                    (entry.blake3.clone(), fs::metadata(path)?.modified()?),
                );
            }
        }
        let expected = manifest
            .directories
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        if expected.len() != manifest.directories.len() {
            return Err(invalid("duplicate add directory inventory"));
        }
        for path in &expected {
            VaultRelativePath::new(path)?;
        }
        let mut actual = BTreeSet::new();
        directory_names(root, root, &mut actual)?;
        if actual != expected
            || count(root, "sources", true)? != manifest.source_count
            || count(root, ".wiki/retained/changes", true)? != manifest.change_count
            || count(root, ".wiki/retained/objects/blake3", false)? != manifest.object_count
        {
            return Err(invalid("add clone S/H/O or directory pin differs"));
        }
        Ok(pins)
    }
    fn require_canonical_pins(root: &Path, pins: &CanonicalPins) -> DiagnosticResult<()> {
        for (relative, (hash, modified)) in pins {
            let path = root.join(relative);
            if &hash_file(&path)? != hash || fs::metadata(path)?.modified()? != *modified {
                return Err(invalid("existing canonical add input changed bytes/mtime"));
            }
        }
        Ok(())
    }
    fn measured(
        name: &str,
        operation: impl FnOnce() -> crate::domain::Result<MutationOutcome>,
    ) -> (crate::domain::Result<MutationOutcome>, Value) {
        profile::begin();
        let utc_start = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|time| time.as_millis());
        let start = Instant::now();
        let outcome = operation();
        let elapsed_ns = start.elapsed().as_nanos();
        let utc_end = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|time| time.as_millis());
        let paths = profile::finish();
        let result = match &outcome {
            Ok(outcome) => json!({"ok":true,"outcome":outcome}),
            Err(error) => json!({"ok":false,"error":{"code":error.code,"message":error.message}}),
        };
        (
            outcome,
            json!({"case":name,"elapsed_ns":elapsed_ns,"paths":paths,"result":result,
            "measurement_window":{"utc_start_unix_ms":utc_start,"utc_end_unix_ms":utc_end},"filesystem_sync_calls":null}),
        )
    }
    fn verify_added(
        manifest: &AddManifest,
        root: &Path,
        catalog: &Catalog,
        outcome: &MutationOutcome,
        before: &crate::domain::ReadSnapshot,
        body: &[u8],
        pins: &CanonicalPins,
    ) -> DiagnosticResult<Value> {
        if outcome.status != Some(ChangeStatus::Committed)
            || outcome.reused
            || outcome.source_capture != Some(SourceCaptureState::Complete)
        {
            return Err(invalid("add profile did not commit complete capture"));
        }
        let source = outcome
            .allocated_ids
            .get("source")
            .ok_or_else(|| invalid("add source ID missing"))?;
        let revision = outcome
            .allocated_ids
            .get("revision")
            .ok_or_else(|| invalid("add revision ID missing"))?;
        if source.as_str().len() != 39 || !source.as_str().bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(invalid("add source ID is not canonical numeric identity"));
        }
        let parent = root.join(format!("sources/{source}/revisions/{revision}"));
        if bounded(&parent.join("original.bin"), 1024 * 1024)? != body
            || bounded(&parent.join("content.md"), 1024 * 1024)? != body
        {
            return Err(invalid("captured immutable original/content differ"));
        }
        let reader = catalog.query_snapshot(QueryReadLimits::default())?;
        let observed = QueryCatalog::snapshot(&reader);
        if observed.generation != before.generation + 1
            || observed.publication().map(|value| &value.file_id)
                != before.publication().map(|value| &value.file_id)
            || outcome.snapshot.as_ref() != Some(observed)
        {
            return Err(invalid("add publication differs from starting epoch"));
        }
        let record = SourceRefreshLookup::unique_record(&reader, source)?
            .ok_or_else(|| invalid("added Source missing"))?;
        if record.record.title() != manifest.title
            || record.record.string("wiki_origin") != Some(manifest.origin.as_str())
            || record.record.string("wiki_origin_kind") != Some("local-file")
            || record.record.string("wiki_current_revision") != Some(revision.as_str())
        {
            return Err(invalid("added Source origin/title/head differs"));
        }
        let change = outcome
            .change
            .as_ref()
            .ok_or_else(|| invalid("add retained proof missing"))?;
        if reader
            .revision_owner(&RevisionTreeKey {
                source_component: source.to_string(),
                revision_component: revision.to_string(),
            })?
            .as_ref()
            != Some(change)
        {
            return Err(invalid("add immutable ownership differs"));
        }
        drop(reader);
        if count(root, "sources", true)? != manifest.source_count + 1
            || count(root, ".wiki/retained/changes", true)? != manifest.change_count + 1
        {
            return Err(invalid(
                "add did not advance exact source/change dimensions",
            ));
        }
        require_canonical_pins(root, pins)?;
        let context = verify_context(
            catalog,
            source,
            revision,
            std::str::from_utf8(body)?,
            &manifest.marker,
        )?;
        if context["snapshot"] != serde_json::to_value(outcome.snapshot.as_ref().unwrap())? {
            return Err(invalid("add context observed another publication"));
        }
        Ok(
            json!({"original_bytes":body.len(),"content_bytes":body.len(),"immutable_hash":Blake3Hash::digest(body),
            "exact_origin":true,"exact_revision_owner":true,"existing_canonical_bytes_mtime":true,"context":context}),
        )
    }

    #[test]
    #[ignore = "requires supervisor-owned pinned source-add clone pair; max2add1apply per invocation"]
    fn profile_normalized_source_add() {
        run_add().unwrap();
    }

    fn run_add() -> DiagnosticResult<()> {
        let manifest_path = env::var_os("LWIKI_SOURCE_ADD_PROFILE_MANIFEST")
            .map(PathBuf::from)
            .ok_or_else(|| invalid("source add manifest required"))?;
        let report_path = env::var_os("LWIKI_SOURCE_ADD_PROFILE_REPORT")
            .map(PathBuf::from)
            .ok_or_else(|| invalid("source add report required"))?;
        absolute(&manifest_path)?;
        absolute(&report_path)?;
        ancestors(
            manifest_path
                .parent()
                .ok_or_else(|| invalid("manifest parent missing"))?,
        )?;
        ancestors(
            report_path
                .parent()
                .ok_or_else(|| invalid("report parent missing"))?,
        )?;
        if fs::symlink_metadata(&report_path).is_ok() {
            return Err(invalid("add report already exists"));
        }
        let bytes = bounded(&manifest_path, 64 * 1024 * 1024)?;
        let manifest: AddManifest = serde_json::from_slice(&bytes)?;
        absolute(&manifest.original_export)?;
        if manifest.version != 1
            || manifest.kind != "disposable-source-add-path-clone-pair"
            || !matches!(
                (
                    manifest.source_count,
                    manifest.change_count,
                    manifest.object_count
                ),
                (10, 31, 91) | (30, 31, 91) | (10, 51, 91) | (10, 31, 131)
            )
            || manifest.whole == manifest.staged
            || manifest.marker != "addprobe999999"
            || manifest.title != "Measured fresh capture"
            || report_path.starts_with(&manifest.original_export)
            || manifest_path.starts_with(&manifest.original_export)
        {
            return Err(invalid("unsupported initial add profile envelope"));
        }
        for root in [&manifest.whole, &manifest.staged] {
            absolute(root)?;
            if root.starts_with(&manifest.original_export)
                || manifest.original_export.starts_with(root)
                || report_path.starts_with(root)
                || manifest_path.starts_with(root)
                || root.starts_with(if root == &manifest.whole {
                    &manifest.staged
                } else {
                    &manifest.whole
                })
            {
                return Err(invalid("add original/clone/report paths must be disjoint"));
            }
        }
        absolute(&manifest.input)?;
        ancestors(
            manifest
                .input
                .parent()
                .ok_or_else(|| invalid("add input parent missing"))?,
        )?;
        if manifest.origin != manifest.input.to_str().unwrap()
            || manifest.input.starts_with(&manifest.whole)
            || manifest.input.starts_with(&manifest.staged)
            || manifest.input.starts_with(&manifest.original_export)
        {
            return Err(invalid("add origin/input must be exact and outside vaults"));
        }
        let body = bounded(&manifest.input, 1024)?;
        if body.len() != 1024
            || Blake3Hash::digest(&body) != manifest.input_blake3
            || std::str::from_utf8(&body)?
                .matches(&manifest.marker)
                .count()
                != 1
        {
            return Err(invalid("add input pin/marker differs"));
        }
        let mut cases = Vec::new();
        let mut starting_snapshot = None;
        let mut starting_authority = Value::Null;
        let result = (|| -> DiagnosticResult<()> {
            validate_clone(&manifest, &manifest.original_export)?;
            let whole_pins = validate_clone(&manifest, &manifest.whole)?;
            let staged_pins = validate_clone(&manifest, &manifest.staged)?;
            let options = OperationOptions {
                offline: true,
                ..Default::default()
            };
            let whole_fs = VaultFs::new(VaultRoot::explicit(&manifest.whole)?);
            let staged_fs = VaultFs::new(VaultRoot::explicit(&manifest.staged)?);
            let whole_app = OfflineApp::new(whole_fs.clone(), options)?;
            let staged_app = OfflineApp::new(
                staged_fs.clone(),
                OperationOptions {
                    stage_only: true,
                    ..options
                },
            )?;
            let apply_app = OfflineApp::new(staged_fs.clone(), options)?;
            let whole_catalog = Catalog::new(whole_fs, whole_app.vault_id().clone());
            let staged_catalog = Catalog::new(staged_fs, staged_app.vault_id().clone());
            let before =
                QueryCatalog::snapshot(&whole_catalog.query_snapshot(QueryReadLimits::default())?)
                    .clone();
            if before.publication().is_none()
                || QueryCatalog::snapshot(
                    &staged_catalog.query_snapshot(QueryReadLimits::default())?,
                ) != &before
            {
                return Err(invalid(
                    "add clones have different/unpublished starting snapshots",
                ));
            }
            let authority = whole_catalog
                .operation_state()?
                .ok_or_else(|| invalid("add clone outside authority missing"))?;
            authority.require_publication(authority.publication())?;
            starting_authority =
                json!({"revision":authority.revision(),"publication":authority.publication()});
            starting_snapshot = Some(before.clone());
            let request = CaptureRequest {
                title: manifest.title.clone(),
                origin_kind: SourceOrigin::LocalFile,
                origin: manifest.origin.clone(),
                original: body.clone(),
                extraction: ExtractionInput::Utf8Preserve,
                media_type: Some("text/plain".into()),
            };
            let (outcome, sample) = measured("whole_add", || whole_app.source_add(request.clone()));
            cases.push(sample);
            let outcome = outcome?;
            cases.last_mut().unwrap()["verification"] = verify_added(
                &manifest,
                &manifest.whole,
                &whole_catalog,
                &outcome,
                &before,
                &body,
                &whole_pins,
            )?;
            let (prepared, sample) = measured("staged_prepare", || staged_app.source_add(request));
            cases.push(sample);
            let prepared = prepared?;
            let source = prepared
                .allocated_ids
                .get("source")
                .ok_or_else(|| invalid("staged source ID missing"))?;
            let source_absent = matches!(fs::symlink_metadata(manifest.staged.join(format!("sources/{source}"))), Err(error) if error.kind() == io::ErrorKind::NotFound);
            if prepared.status != Some(ChangeStatus::Prepared)
                || prepared.reused
                || prepared.source_capture != Some(SourceCaptureState::Complete)
                || prepared.snapshot.is_some()
                || !source_absent
                || QueryCatalog::snapshot(
                    &staged_catalog.query_snapshot(QueryReadLimits::default())?,
                ) != &before
            {
                return Err(invalid("staged prepare changed canonical/selected state"));
            }
            require_canonical_pins(&manifest.staged, &staged_pins)?;
            let change = prepared
                .change
                .as_ref()
                .ok_or_else(|| invalid("staged retained proof missing"))?
                .change_id
                .clone();
            let (applied, sample) = measured("staged_apply", || apply_app.changes_apply(change));
            cases.push(sample);
            let mut applied = applied?;
            if applied.allocated_ids != prepared.allocated_ids || applied.change != prepared.change
            {
                return Err(invalid("staged apply differs from prepared identity/proof"));
            }
            // changes_apply reports no extraction work; retain the capture state's
            // original prepared observation for the common final verifier.
            applied.source_capture = prepared.source_capture;
            cases.last_mut().unwrap()["verification"] = verify_added(
                &manifest,
                &manifest.staged,
                &staged_catalog,
                &applied,
                &before,
                &body,
                &staged_pins,
            )?;
            validate_clone(&manifest, &manifest.original_export)?;
            Ok(())
        })();
        let report = json!({"version":1,"kind":"source-add-path-attribution","counter_grouping_version":2,
            "complete":result.is_ok(),"error":result.as_ref().err().map(|error|error.to_string()),
            "manifest_blake3":Blake3Hash::digest(&bytes),"test_binary_blake3":hash_file(&env::current_exe()?)?,
            "source_count":manifest.source_count,"change_count":manifest.change_count,"object_count":manifest.object_count,
            "starting_snapshot":starting_snapshot,"starting_authority":starting_authority,
            "whole":manifest.whole,"staged":manifest.staged,"original_export":manifest.original_export,"cases":cases,
            "source_add_limit":2,"changes_apply_limit":1,"filesystem_sync_calls":null,
            "timing_note":"Direct OfflineApp excludes CLI/config. Whole versus staged prepare/apply are separate runs, not additive attribution. Portable/sub-enumeration and immutable timers are inclusive. Parent inventory/ps overhead must be reported separately. Verification/check outside counters; no capacity qualification."});
        let bytes = serde_json::to_vec_pretty(&report)?;
        if bytes.len() > 1024 * 1024 {
            return Err(invalid("add profile report ceiling"));
        }
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&report_path)?;
        output.write_all(&bytes)?;
        output.write_all(b"\n")?;
        output.sync_all()?;
        println!(
            "{}",
            serde_json::to_string(
                &json!({"profile_report":report_path,"complete":result.is_ok()})
            )?
        );
        result
    }
    /// Frozen one-pair extension; the original L2 harness above is unchanged.
    mod sync_attribution {
        use super::*;
        use crate::vault::{DirectorySync, DurableIo, NativeIo};
        use std::{
            fs::File,
            sync::{Arc, Mutex},
        };

        const COVERAGE: &str = "Only syncs routed through this VaultFs/DurableIo Arc. SQLite internal syncs and direct File::sync_all/NativeIo bypasses are excluded. Sequential sync timers exclude counter mutex/allocation work; outer timers include instrumentation overhead. Not total fsync, latency distribution or capacity qualification.";

        #[derive(Default, Clone, serde::Serialize)]
        struct SyncCalls {
            calls: u64,
            completed: u64,
            errors: u64,
            unsupported: u64,
            elapsed_ns: u128,
        }
        #[derive(Default, Clone, serde::Serialize)]
        struct SyncProfile {
            file: SyncCalls,
            directory: SyncCalls,
        }
        #[derive(Default)]
        struct CountingNativeIo {
            active: Mutex<Option<SyncProfile>>,
        }
        impl CountingNativeIo {
            fn begin(&self) {
                let mut active = self.active.lock().unwrap();
                assert!(active.is_none(), "sync scope already active");
                *active = Some(SyncProfile::default());
            }
            fn finish(&self) -> SyncProfile {
                self.active
                    .lock()
                    .unwrap()
                    .take()
                    .expect("sync scope inactive")
            }
            fn record(
                &self,
                directory: bool,
                elapsed_ns: u128,
                completed: bool,
                unsupported: bool,
            ) {
                if let Some(profile) = self.active.lock().unwrap().as_mut() {
                    let calls = if directory {
                        &mut profile.directory
                    } else {
                        &mut profile.file
                    };
                    calls.calls += 1;
                    calls.completed += u64::from(completed);
                    calls.errors += u64::from(!completed);
                    calls.unsupported += u64::from(unsupported);
                    calls.elapsed_ns += elapsed_ns;
                }
            }
        }
        impl DurableIo for CountingNativeIo {
            fn create_stage(&self, path: &Path) -> io::Result<File> {
                NativeIo.create_stage(path)
            }
            fn create_private_stage(&self, path: &Path) -> io::Result<File> {
                NativeIo.create_private_stage(path)
            }
            fn create_private_directory(&self, path: &Path) -> io::Result<()> {
                NativeIo.create_private_directory(path)
            }
            fn open_append(&self, path: &Path) -> io::Result<File> {
                NativeIo.open_append(path)
            }
            fn truncate_file(&self, file: &File, length: u64) -> io::Result<()> {
                NativeIo.truncate_file(file, length)
            }
            fn write_stage(&self, file: &mut File, bytes: &[u8]) -> io::Result<()> {
                NativeIo.write_stage(file, bytes)
            }
            fn sync_file(&self, file: &File) -> io::Result<()> {
                let started = Instant::now();
                let result = NativeIo.sync_file(file);
                let elapsed_ns = started.elapsed().as_nanos();
                self.record(
                    false,
                    elapsed_ns,
                    result.is_ok(),
                    result
                        .as_ref()
                        .err()
                        .is_some_and(|error| error.kind() == io::ErrorKind::Unsupported),
                );
                result
            }
            fn replace(&self, staged: &Path, target: &Path) -> io::Result<()> {
                NativeIo.replace(staged, target)
            }
            fn remove(&self, target: &Path) -> io::Result<()> {
                NativeIo.remove(target)
            }
            fn remove_directory(&self, target: &Path) -> io::Result<()> {
                NativeIo.remove_directory(target)
            }
            fn create_directory(&self, path: &Path) -> io::Result<()> {
                NativeIo.create_directory(path)
            }
            fn sync_directory(&self, path: &Path) -> io::Result<DirectorySync> {
                let started = Instant::now();
                let result = NativeIo.sync_directory(path);
                let elapsed_ns = started.elapsed().as_nanos();
                self.record(
                    true,
                    elapsed_ns,
                    result.is_ok(),
                    matches!(&result, Ok(DirectorySync::Unsupported))
                        || result
                            .as_ref()
                            .err()
                            .is_some_and(|error| error.kind() == io::ErrorKind::Unsupported),
                );
                result
            }
        }
        fn measured_sync(
            name: &str,
            adapter: &CountingNativeIo,
            operation: impl FnOnce() -> crate::domain::Result<MutationOutcome>,
        ) -> (crate::domain::Result<MutationOutcome>, Value) {
            adapter.begin();
            let (result, mut sample) = measured(name, operation);
            let sync = adapter.finish();
            sample["vaultfs_sync"] =
                json!({"file":sync.file,"directory":sync.directory,"coverage":COVERAGE});
            (result, sample)
        }
        fn path_counts(value: &Value) -> DiagnosticResult<Value> {
            let mut paths = value
                .get("paths")
                .cloned()
                .ok_or_else(|| invalid("baseline paths missing"))?;
            let object = paths
                .as_object_mut()
                .ok_or_else(|| invalid("baseline paths invalid"))?;
            object.remove("portable_elapsed_ns");
            let enums = object
                .get_mut("enumerations")
                .and_then(Value::as_object_mut)
                .ok_or_else(|| invalid("baseline enumerations invalid"))?;
            for value in enums.values_mut() {
                value
                    .as_object_mut()
                    .ok_or_else(|| invalid("baseline enumeration invalid"))?
                    .remove("elapsed_ns");
            }
            Ok(paths)
        }
        fn require_arc(fs: &VaultFs, expected: &Arc<dyn DurableIo>) -> DiagnosticResult<()> {
            if !Arc::ptr_eq(&fs.durable_io(), expected) {
                return Err(invalid("sync adapter Arc was replaced"));
            }
            Ok(())
        }
        #[test]
        fn native_sync_scope_preserves_clones_errors_and_private_creation() {
            let temp = tempfile::tempdir().unwrap();
            fs::write(
                temp.path().join("WIKI.md"),
                include_bytes!("../../tests/fixtures/bootstrap/vault/WIKI.md"),
            )
            .unwrap();
            let adapter = Arc::new(CountingNativeIo::default());
            let erased: Arc<dyn DurableIo> = adapter.clone();
            let handle =
                VaultFs::with_io(VaultRoot::explicit(temp.path()).unwrap(), erased.clone());
            let cloned = handle.clone();
            require_arc(&cloned, &erased).unwrap();
            let private = temp.path().join("private");
            erased.create_private_directory(&private).unwrap();
            let path = private.join("bytes");
            let mut file = erased.create_private_stage(&path).unwrap();
            erased.write_stage(&mut file, b"exact bytes").unwrap();
            adapter.begin();
            cloned.durable_io().sync_file(&file).unwrap();
            let directory = cloned.durable_io().sync_directory(&private).unwrap();
            let missing = private.join("absent");
            let native_error = NativeIo.sync_directory(&missing).unwrap_err();
            let observed_error = cloned.durable_io().sync_directory(&missing).unwrap_err();
            assert_eq!(observed_error.kind(), native_error.kind());
            assert_eq!(observed_error.raw_os_error(), native_error.raw_os_error());
            let counts = adapter.finish();
            assert_eq!(
                (counts.file.calls, counts.file.completed, counts.file.errors),
                (1, 1, 0)
            );
            assert_eq!(
                (
                    counts.directory.calls,
                    counts.directory.completed,
                    counts.directory.errors
                ),
                (2, 1, 1)
            );
            assert_eq!(
                counts.directory.unsupported,
                u64::from(directory == DirectorySync::Unsupported)
            );
            assert_eq!(fs::read(&path).unwrap(), b"exact bytes");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
                assert_eq!(
                    fs::metadata(&private).unwrap().permissions().mode() & 0o777,
                    0o700
                );
            }
            // Outside scopes still forwards; the next scope excludes those calls.
            erased.sync_file(&file).unwrap();
            adapter.begin();
            assert_eq!(adapter.finish().file.calls, 0);
        }
        #[test]
        #[ignore = "one supervisor-owned existing baseline clone pair; exactly2add1apply"]
        fn profile_normalized_source_add_sync() {
            run_sync().unwrap();
        }
        fn run_sync() -> DiagnosticResult<()> {
            let manifest_path = env::var_os("LWIKI_SOURCE_ADD_PROFILE_MANIFEST")
                .map(PathBuf::from)
                .ok_or_else(|| invalid("source add manifest required"))?;
            let report_path = env::var_os("LWIKI_SOURCE_ADD_PROFILE_REPORT")
                .map(PathBuf::from)
                .ok_or_else(|| invalid("source add report required"))?;
            absolute(&manifest_path)?;
            absolute(&report_path)?;
            ancestors(
                manifest_path
                    .parent()
                    .ok_or_else(|| invalid("manifest parent missing"))?,
            )?;
            ancestors(
                report_path
                    .parent()
                    .ok_or_else(|| invalid("report parent missing"))?,
            )?;
            if fs::symlink_metadata(&report_path).is_ok() {
                return Err(invalid("add report already exists"));
            }
            let bytes = bounded(&manifest_path, 64 * 1024 * 1024)?;
            let manifest: AddManifest = serde_json::from_slice(&bytes)?;
            absolute(&manifest.original_export)?;
            if manifest.version != 1
                || manifest.kind != "disposable-source-add-sync-clone-pair"
                || !matches!(
                    (
                        manifest.source_count,
                        manifest.change_count,
                        manifest.object_count
                    ),
                    (10, 31, 91)
                )
                || manifest.whole == manifest.staged
                || manifest.marker != "addprobe999999"
                || manifest.title != "Measured fresh capture"
                || report_path.starts_with(&manifest.original_export)
                || manifest_path.starts_with(&manifest.original_export)
            {
                return Err(invalid("unsupported initial add profile envelope"));
            }
            for root in [&manifest.whole, &manifest.staged] {
                absolute(root)?;
                if root.starts_with(&manifest.original_export)
                    || manifest.original_export.starts_with(root)
                    || report_path.starts_with(root)
                    || manifest_path.starts_with(root)
                    || root.starts_with(if root == &manifest.whole {
                        &manifest.staged
                    } else {
                        &manifest.whole
                    })
                {
                    return Err(invalid("add original/clone/report paths must be disjoint"));
                }
            }
            absolute(&manifest.input)?;
            ancestors(
                manifest
                    .input
                    .parent()
                    .ok_or_else(|| invalid("add input parent missing"))?,
            )?;
            if manifest.origin != manifest.input.to_str().unwrap()
                || manifest.input.starts_with(&manifest.whole)
                || manifest.input.starts_with(&manifest.staged)
                || manifest.input.starts_with(&manifest.original_export)
            {
                return Err(invalid("add origin/input must be exact and outside vaults"));
            }
            let body = bounded(&manifest.input, 1024)?;
            if body.len() != 1024
                || Blake3Hash::digest(&body) != manifest.input_blake3
                || std::str::from_utf8(&body)?
                    .matches(&manifest.marker)
                    .count()
                    != 1
            {
                return Err(invalid("add input pin/marker differs"));
            }
            let baseline_path = env::var_os("LWIKI_SOURCE_ADD_SYNC_BASELINE")
                .map(PathBuf::from)
                .ok_or_else(|| invalid("sync baseline required"))?;
            absolute(&baseline_path)?;
            ancestors(
                baseline_path
                    .parent()
                    .ok_or_else(|| invalid("sync baseline parent missing"))?,
            )?;
            if baseline_path.starts_with(&manifest.whole)
                || baseline_path.starts_with(&manifest.staged)
                || baseline_path.starts_with(&manifest.original_export)
            {
                return Err(invalid("sync baseline must be outside all vaults"));
            }
            let baseline_bytes = bounded(&baseline_path, 1024 * 1024)?;
            let baseline: Value = serde_json::from_slice(&baseline_bytes)?;
            if baseline["kind"] != "source-add-path-attribution"
                || baseline["complete"] != true
                || baseline["source_count"] != 10
                || baseline["change_count"] != 31
                || baseline["object_count"] != 91
                || baseline["cases"].as_array().map(Vec::len) != Some(3)
            {
                return Err(invalid("sync baseline differs from frozen initial case"));
            }
            let mut cases = Vec::new();
            let mut starting_snapshot = None;
            let mut starting_authority = Value::Null;
            let mut adapter_arc_verified = false;
            let result = (|| -> DiagnosticResult<()> {
                validate_clone(&manifest, &manifest.original_export)?;
                let whole_pins = validate_clone(&manifest, &manifest.whole)?;
                let staged_pins = validate_clone(&manifest, &manifest.staged)?;
                let options = OperationOptions {
                    offline: true,
                    ..Default::default()
                };
                let whole_io = Arc::new(CountingNativeIo::default());
                let staged_io = Arc::new(CountingNativeIo::default());
                let whole_erased: Arc<dyn DurableIo> = whole_io.clone();
                let staged_erased: Arc<dyn DurableIo> = staged_io.clone();
                let whole_fs =
                    VaultFs::with_io(VaultRoot::explicit(&manifest.whole)?, whole_erased.clone());
                let staged_fs = VaultFs::with_io(
                    VaultRoot::explicit(&manifest.staged)?,
                    staged_erased.clone(),
                );
                let whole_app = OfflineApp::new(whole_fs.clone(), options)?;
                let staged_app = OfflineApp::new(
                    staged_fs.clone(),
                    OperationOptions {
                        stage_only: true,
                        ..options
                    },
                )?;
                let apply_app = OfflineApp::new(staged_fs.clone(), options)?;
                let whole_catalog = Catalog::new(whole_fs, whole_app.vault_id().clone());
                let staged_catalog = Catalog::new(staged_fs, staged_app.vault_id().clone());
                require_arc(whole_app.fs(), &whole_erased)?;
                require_arc(staged_app.fs(), &staged_erased)?;
                require_arc(apply_app.fs(), &staged_erased)?;
                require_arc(&whole_catalog.fs, &whole_erased)?;
                require_arc(&staged_catalog.fs, &staged_erased)?;
                adapter_arc_verified = true;
                let before = QueryCatalog::snapshot(
                    &whole_catalog.query_snapshot(QueryReadLimits::default())?,
                )
                .clone();
                if before.publication().is_none()
                    || QueryCatalog::snapshot(
                        &staged_catalog.query_snapshot(QueryReadLimits::default())?,
                    ) != &before
                {
                    return Err(invalid(
                        "add clones have different/unpublished starting snapshots",
                    ));
                }
                let authority = whole_catalog
                    .operation_state()?
                    .ok_or_else(|| invalid("add clone outside authority missing"))?;
                authority.require_publication(authority.publication())?;
                starting_authority =
                    json!({"revision":authority.revision(),"publication":authority.publication()});
                starting_snapshot = Some(before.clone());
                let request = CaptureRequest {
                    title: manifest.title.clone(),
                    origin_kind: SourceOrigin::LocalFile,
                    origin: manifest.origin.clone(),
                    original: body.clone(),
                    extraction: ExtractionInput::Utf8Preserve,
                    media_type: Some("text/plain".into()),
                };
                let (outcome, sample) = measured_sync("whole_add", &whole_io, || {
                    whole_app.source_add(request.clone())
                });
                cases.push(sample);
                let outcome = outcome?;
                cases.last_mut().unwrap()["verification"] = verify_added(
                    &manifest,
                    &manifest.whole,
                    &whole_catalog,
                    &outcome,
                    &before,
                    &body,
                    &whole_pins,
                )?;
                let (prepared, sample) = measured_sync("staged_prepare", &staged_io, || {
                    staged_app.source_add(request)
                });
                cases.push(sample);
                let prepared = prepared?;
                let source = prepared
                    .allocated_ids
                    .get("source")
                    .ok_or_else(|| invalid("staged source ID missing"))?;
                let source_absent = matches!(fs::symlink_metadata(manifest.staged.join(format!("sources/{source}"))), Err(error) if error.kind() == io::ErrorKind::NotFound);
                if prepared.status != Some(ChangeStatus::Prepared)
                    || prepared.reused
                    || prepared.source_capture != Some(SourceCaptureState::Complete)
                    || prepared.snapshot.is_some()
                    || !source_absent
                    || QueryCatalog::snapshot(
                        &staged_catalog.query_snapshot(QueryReadLimits::default())?,
                    ) != &before
                {
                    return Err(invalid("staged prepare changed canonical/selected state"));
                }
                require_canonical_pins(&manifest.staged, &staged_pins)?;
                let change = prepared
                    .change
                    .as_ref()
                    .ok_or_else(|| invalid("staged retained proof missing"))?
                    .change_id
                    .clone();
                let (applied, sample) = measured_sync("staged_apply", &staged_io, || {
                    apply_app.changes_apply(change)
                });
                cases.push(sample);
                let mut applied = applied?;
                if applied.allocated_ids != prepared.allocated_ids
                    || applied.change != prepared.change
                {
                    return Err(invalid("staged apply differs from prepared identity/proof"));
                }
                // changes_apply reports no extraction work; retain the capture state's
                // original prepared observation for the common final verifier.
                applied.source_capture = prepared.source_capture;
                cases.last_mut().unwrap()["verification"] = verify_added(
                    &manifest,
                    &manifest.staged,
                    &staged_catalog,
                    &applied,
                    &before,
                    &body,
                    &staged_pins,
                )?;
                validate_clone(&manifest, &manifest.original_export)?;
                for (index, sample) in cases.iter().enumerate() {
                    if sample["case"] != baseline["cases"][index]["case"]
                        || path_counts(sample)? != path_counts(&baseline["cases"][index])?
                    {
                        return Err(invalid(
                            "sync instrumentation changed frozen baseline PathProfile counts",
                        ));
                    }
                    if sample["vaultfs_sync"]["file"]["calls"]
                        .as_u64()
                        .unwrap_or(0)
                        == 0
                        || sample["vaultfs_sync"]["directory"]["calls"]
                            .as_u64()
                            .unwrap_or(0)
                            == 0
                    {
                        return Err(invalid("expected real VaultFs sync calls absent"));
                    }
                }
                Ok(())
            })();
            let report = json!({"version":1,"kind":"source-add-sync-attribution","counter_grouping_version":2,
                "complete":result.is_ok(),"error":result.as_ref().err().map(|error|error.to_string()),
                "manifest_blake3":Blake3Hash::digest(&bytes),"test_binary_blake3":hash_file(&env::current_exe()?)?,
                "source_count":manifest.source_count,"change_count":manifest.change_count,"object_count":manifest.object_count,
                "starting_snapshot":starting_snapshot,"starting_authority":starting_authority,
                "whole":manifest.whole,"staged":manifest.staged,"original_export":manifest.original_export,"cases":cases,
                "source_add_limit":2,"changes_apply_limit":1,"filesystem_sync_calls":null,"sync_coverage":COVERAGE,"adapter_arc_verified":adapter_arc_verified,"baseline_profile_blake3":Blake3Hash::digest(&baseline_bytes),"baseline_path_counts_match":result.is_ok(),
                "timing_note":"Direct OfflineApp excludes CLI/config. Whole versus staged prepare/apply are separate runs, not additive attribution. Portable/sub-enumeration and immutable timers are inclusive. Parent inventory/ps overhead must be reported separately. Verification/check outside counters; no capacity qualification."});
            let bytes = serde_json::to_vec_pretty(&report)?;
            if bytes.len() > 1024 * 1024 {
                return Err(invalid("add profile report ceiling"));
            }
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&report_path)?;
            output.write_all(&bytes)?;
            output.write_all(b"\n")?;
            output.sync_all()?;
            println!(
                "{}",
                serde_json::to_string(
                    &json!({"profile_report":report_path,"complete":result.is_ok()})
                )?
            );
            result
        }
    }
}

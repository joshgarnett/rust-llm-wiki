//! Explicit ignored native exports and one finite paired counting diagnostic.
//! Root wires this as a child of source_import_tests; ordinary tests never run it.
use super::*;
use crate::catalog::query_types::QueryCatalog;
use serde::Serialize;
use std::{env, fs::OpenOptions, io, process::Command, time::Instant};

const COVERAGE: &str = "DurableIo adapter calls only; excludes SQLite internal syncs, writer-lock NativeIo bypasses, standalone manifest File::sync_all/NativeIo and other direct filesystem calls. Whole-process Instant interval includes preparation/hash/state/results/verified cited readiness and instrumentation; no latency speedup, total fsync or capacity claim.";
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn fail(message: &str) -> Box<dyn std::error::Error> {
    io::Error::other(message).into()
}
fn requested_directory(name: &str) -> Result<PathBuf> {
    let path = PathBuf::from(
        env::var_os(name).ok_or_else(|| fail("explicit artifact directory required"))?,
    );
    if !path.is_absolute() || path.exists() {
        return Err(fail("artifact directory must be absolute and unoccupied"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| fail("artifact directory has no parent"))?;
    let canonical = fs::canonicalize(parent)?;
    if canonical != parent {
        return Err(fail("artifact directory parent must be canonical"));
    }
    fs::create_dir(&path)?;
    Ok(path)
}
fn write_json(path: &Path, value: &Value) -> Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    NativeIo.sync_directory(path.parent().unwrap())?;
    Ok(())
}
fn pending_descriptor(fixture: &Fixture) -> Result<Value> {
    let app = fixture.app();
    let store = super::super::source_import_state::ImportStore::new(
        app.fs().clone(),
        app.vault_id().clone(),
        KEY,
    )?;
    let (progress, _) = store
        .load()?
        .ok_or_else(|| fail("export lacks known progress"))?;
    let pending = progress
        .pending
        .as_ref()
        .ok_or_else(|| fail("export lacks pending group"))?;
    let details = app.changes_show(pending.change.change_id.clone())?;
    Ok(
        json!({"key":KEY,"manifest_hash":progress.manifest.hash,"pending":pending,"status":details.status,"prepared":details.prepared,"originals":fixture.bytes.iter().enumerate().map(|(ordinal,bytes)|json!({"ordinal":ordinal,"bytes":bytes,"hash":Blake3Hash::digest(bytes)})).collect::<Vec<_>>()}),
    )
}
fn export_fixture(
    fixture: Fixture,
    destination: &Path,
    name: &str,
    cut: &str,
    cut_fired: bool,
) -> Result<Value> {
    if !cut_fired {
        return Err(fail("native returned-error cut did not fire"));
    }
    let mut descriptor = pending_descriptor(&fixture)?;
    descriptor["case"] = json!(name);
    descriptor["cut"] = json!(cut);
    descriptor["cut_fired"] = json!(true);
    descriptor["original_fixture_temp"] = json!(fixture.temp.path());
    descriptor["original_paths_missing"] = json!(fixture.inputs.iter().all(|path| !path.exists()));
    let vault_name = fixture.root.file_name().unwrap().to_owned();
    let target = destination.join(name);
    let kept = fixture.temp.keep();
    // Preserve complete native state before any replay. Embedded original paths
    // intentionally become missing; no manifest or authority is hand-edited.
    fs::rename(&kept, &target)?;
    NativeIo.sync_directory(destination)?;
    descriptor["fixture_root"] = json!(target);
    descriptor["vault"] = json!(target.join(vault_name));
    descriptor["expected_replay"] = json!(
        "source import resume --key; same Source/Revision/time mapping; same Change except stale Aborted replacement"
    );
    write_json(&destination.join(format!("{name}.json")), &descriptor)?;
    Ok(descriptor)
}

#[test]
#[ignore = "explicit LWIKI_IMPORT_EXPORT_DIR; four authentic cuts, preserve before critic replay"]
fn export_native_import_failure_cuts() {
    let destination = requested_directory("LWIKI_IMPORT_EXPORT_DIR").unwrap();
    let mut descriptors = vec![];
    for (name, migrated, cut) in [
        ("schema1-prepared-no-proof", false, Cut::BeforeProof),
        (
            "schema2-visible-begin-before-applying",
            true,
            Cut::AfterBegin,
        ),
        (
            "schema2-complete-result-sync-error",
            true,
            Cut::AfterResultAppend,
        ),
    ] {
        let fixture = Fixture::new(migrated, 4);
        fixture.prepare();
        let adapter = Arc::new(ImportIo::new(Some(cut), vec![]));
        let app = OfflineApp::new(
            VaultFs::with_io(VaultRoot::explicit(&fixture.root).unwrap(), adapter.clone()),
            options(),
        )
        .unwrap();
        assert!(app.source_import_run(&fixture.manifest, KEY, 4, 1).is_err());
        let fired = adapter.fired.load(Ordering::SeqCst);
        drop(app);
        fixture.remove_originals();
        descriptors
            .push(export_fixture(fixture, &destination, name, &format!("{cut:?}"), fired).unwrap());
    }
    let fixture = Fixture::new(false, 4);
    fixture.prepare();
    let (old, _) = prepared_import(&fixture, Cut::BeforeProof);
    advance_import_base(&fixture); // Actual managed publication, no authority edit.
    fixture.remove_originals();
    let adapter = Arc::new(ImportIo::new(Some(Cut::BeforeResultAppend), vec![]));
    let app = OfflineApp::new(
        VaultFs::with_io(VaultRoot::explicit(&fixture.root).unwrap(), adapter.clone()),
        options(),
    )
    .unwrap();
    assert!(app.source_import_resume(KEY, 1).is_err());
    assert!(adapter.fired.load(Ordering::SeqCst));
    assert_eq!(
        app.changes_show(old.change_id).unwrap().status,
        ChangeStatus::Aborted
    );
    drop(app);
    descriptors.push(
        export_fixture(
            fixture,
            &destination,
            "schema1-proofless-aborted-lost-close",
            "BeforeProof then managed publication then BeforeResultAppend during stale closure",
            true,
        )
        .unwrap(),
    );
    write_json(&destination.join("exports.json"), &json!({"version":1,"kind":"native-import-returned-error-exports","cases":descriptors,"replayed":false,"limits":"four items per case; one real unrelated capture solely for stale-base correctness; not diagnostic measurement captures"})).unwrap();
}

#[derive(Default, Clone, Serialize)]
struct Calls {
    calls: u64,
    completed: u64,
    errors: u64,
    unsupported: u64,
    bytes: u64,
    elapsed_ns: u128,
}
#[derive(Default)]
struct CountingNativeIo {
    active: Mutex<Option<BTreeMap<&'static str, Calls>>>,
}
impl CountingNativeIo {
    fn begin(&self) {
        let mut state = self.active.lock().unwrap();
        assert!(state.is_none());
        *state = Some(BTreeMap::new());
    }
    fn finish(&self) -> BTreeMap<&'static str, Calls> {
        self.active
            .lock()
            .unwrap()
            .take()
            .expect("counting scope inactive")
    }
    fn call<T>(
        &self,
        name: &'static str,
        bytes: usize,
        operation: impl FnOnce() -> io::Result<T>,
        unsupported: impl FnOnce(&io::Result<T>) -> bool,
    ) -> io::Result<T> {
        let started = Instant::now();
        let result = operation();
        let elapsed_ns = started.elapsed().as_nanos();
        let unsupported = unsupported(&result);
        if let Some(state) = self.active.lock().unwrap().as_mut() {
            let entry = state.entry(name).or_default();
            entry.calls += 1;
            entry.completed += u64::from(result.is_ok());
            entry.errors += u64::from(result.is_err());
            entry.unsupported += u64::from(unsupported);
            entry.bytes += bytes as u64;
            entry.elapsed_ns += elapsed_ns;
        }
        result
    }
}
fn unsupported<T>(result: &io::Result<T>) -> bool {
    result
        .as_ref()
        .err()
        .is_some_and(|error| error.kind() == io::ErrorKind::Unsupported)
}
impl DurableIo for CountingNativeIo {
    fn create_stage(&self, p: &Path) -> io::Result<File> {
        self.call("create_stage", 0, || NativeIo.create_stage(p), unsupported)
    }
    fn create_private_stage(&self, p: &Path) -> io::Result<File> {
        self.call(
            "create_private_stage",
            0,
            || NativeIo.create_private_stage(p),
            unsupported,
        )
    }
    fn create_private_directory(&self, p: &Path) -> io::Result<()> {
        self.call(
            "create_private_directory",
            0,
            || NativeIo.create_private_directory(p),
            unsupported,
        )
    }
    fn open_append(&self, p: &Path) -> io::Result<File> {
        self.call("open_append", 0, || NativeIo.open_append(p), unsupported)
    }
    fn truncate_file(&self, f: &File, n: u64) -> io::Result<()> {
        self.call(
            "truncate_file",
            0,
            || NativeIo.truncate_file(f, n),
            unsupported,
        )
    }
    fn write_stage(&self, f: &mut File, b: &[u8]) -> io::Result<()> {
        self.call(
            "write_stage",
            b.len(),
            || NativeIo.write_stage(f, b),
            unsupported,
        )
    }
    fn sync_file(&self, f: &File) -> io::Result<()> {
        self.call("sync_file", 0, || NativeIo.sync_file(f), unsupported)
    }
    fn replace(&self, a: &Path, b: &Path) -> io::Result<()> {
        self.call("replace", 0, || NativeIo.replace(a, b), unsupported)
    }
    fn remove(&self, p: &Path) -> io::Result<()> {
        self.call("remove", 0, || NativeIo.remove(p), unsupported)
    }
    fn remove_directory(&self, p: &Path) -> io::Result<()> {
        self.call(
            "remove_directory",
            0,
            || NativeIo.remove_directory(p),
            unsupported,
        )
    }
    fn create_directory(&self, p: &Path) -> io::Result<()> {
        self.call(
            "create_directory",
            0,
            || NativeIo.create_directory(p),
            unsupported,
        )
    }
    fn sync_directory(&self, p: &Path) -> io::Result<DirectorySync> {
        self.call(
            "sync_directory",
            0,
            || NativeIo.sync_directory(p),
            |result| unsupported(result) || matches!(result, Ok(DirectorySync::Unsupported)),
        )
    }
}

#[derive(Clone, Serialize)]
struct Pin {
    bytes: u64,
    blake3: Blake3Hash,
    modified_ns: u128,
    mode: u32,
}
fn inventory(root: &Path) -> Result<BTreeMap<PathBuf, Pin>> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    fn visit(
        root: &Path,
        directory: &Path,
        result: &mut BTreeMap<PathBuf, Pin>,
        bytes: &mut u64,
    ) -> Result<()> {
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Err(fail("fixture inventory refuses symlinks"));
            }
            if metadata.is_dir() {
                visit(root, &path, result, bytes)?;
            } else if metadata.is_file() {
                if metadata.nlink() != 1 {
                    return Err(fail("fixture inventory refuses hardlinks"));
                }
                *bytes = bytes
                    .checked_add(metadata.len())
                    .ok_or_else(|| fail("inventory length overflow"))?;
                if *bytes > 64 * 1024 * 1024 || result.len() >= 4096 {
                    return Err(fail("fixture inventory exceeds64MiB/4096files"));
                }
                let body = fs::read(&path)?;
                result.insert(
                    path.strip_prefix(root)?.to_owned(),
                    Pin {
                        bytes: metadata.len(),
                        blake3: Blake3Hash::digest(body),
                        modified_ns: metadata
                            .modified()?
                            .duration_since(SystemTime::UNIX_EPOCH)?
                            .as_nanos(),
                        mode: metadata.permissions().mode(),
                    },
                );
            } else {
                return Err(fail("nonregular fixture entry"));
            }
        }
        Ok(())
    }
    let mut result = BTreeMap::new();
    visit(root, root, &mut result, &mut 0)?;
    Ok(result)
}
fn clone_tree(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir(target)?;
    for entry in fs::read_dir(source)? {
        let path = entry?.path();
        let metadata = fs::symlink_metadata(&path)?;
        let destination = target.join(path.file_name().unwrap());
        if metadata.file_type().is_symlink() {
            return Err(fail("baseline copy refuses symlinks"));
        }
        if metadata.is_dir() {
            clone_tree(&path, &destination)?;
        } else if metadata.is_file() {
            fs::copy(&path, &destination)?;
            fs::set_permissions(&destination, metadata.permissions())?;
            File::options()
                .write(true)
                .open(&destination)?
                .set_times(fs::FileTimes::new().set_modified(metadata.modified()?))?;
        } else {
            return Err(fail("baseline copy refuses nonregular entry"));
        }
    }
    Ok(())
}
fn sha256(paths: &[PathBuf]) -> Result<Value> {
    // Explicit native utility, no shell interpolation; hashes outside intervals.
    let output = Command::new("/usr/bin/shasum")
        .args(["-a", "256", "--"])
        .args(paths)
        .output()?;
    if !output.status.success() {
        return Err(fail("native SHA256 utility failed"));
    }
    let stdout = String::from_utf8(output.stdout)?;
    let lines: Vec<_> = stdout.lines().collect();
    if lines.len() != paths.len() {
        return Err(fail("SHA256 output count differs"));
    }
    let mut hashes = serde_json::Map::new();
    for (path, line) in paths.iter().zip(lines) {
        let hash = line.get(..64).ok_or_else(|| fail("short SHA256 output"))?;
        if !hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(fail("invalid SHA256 output"));
        }
        hashes.insert(
            path.to_str()
                .ok_or_else(|| fail("nonUTF8 SHA256 path"))?
                .into(),
            json!(hash),
        );
    }
    Ok(Value::Object(hashes))
}
fn counts_and_snapshot(root: &Path) -> Result<Value> {
    let app = OfflineApp::new(VaultFs::new(VaultRoot::explicit(root)?), options())?;
    let catalog = Catalog::new(app.fs().clone(), app.vault_id().clone());
    let authority = catalog
        .operation_state()?
        .ok_or_else(|| fail("baseline lacks normalized authority"))?;
    if authority.active().is_some() {
        return Err(fail("baseline must be idle"));
    }
    catalog.guard_query()?;
    let reader = catalog.query_snapshot(QueryReadLimits::default())?;
    if !reader.normalized_layout() {
        return Err(fail("baseline must have current normalized catalog"));
    }
    let count = |path: VaultRelativePath, directories: bool| -> Result<usize> {
        let mut total = 0;
        for entry in fs::read_dir(app.fs().root().resolve(&path)?)? {
            let kind = entry?.file_type()?;
            total += usize::from(if directories {
                kind.is_dir()
            } else {
                kind.is_file()
            });
        }
        Ok(total)
    };
    Ok(
        json!({"sources":count(rel("sources"),true)?,"changes":count(rel(".wiki/retained/changes"),true)?,"canonical_changes":count(rel("changes"),false)?,"objects":count(rel(".wiki/retained/objects/blake3"),false)?,"snapshot":reader.snapshot(),"idle":true,"normalized":true}),
    )
}
fn cited_readiness(app: &OfflineApp, items: &[Value], bodies: &[Vec<u8>]) -> Result<Value> {
    let catalog = Catalog::new(app.fs().clone(), app.vault_id().clone());
    let mut ready = vec![];
    for item in items {
        let ordinal = item["ordinal"]
            .as_u64()
            .ok_or_else(|| fail("mapping ordinal missing"))? as usize;
        let source: RecordId = serde_json::from_value(item["source_id"].clone())?;
        let revision: RecordId = serde_json::from_value(item["revision_id"].clone())?;
        let read = app.read(ReadRequest {
            selector: RecordSelector::Path(rel(format!(
                "sources/{source}/revisions/{revision}/content.md"
            ))),
            range: None,
            max_bytes: 4096,
        })?;
        if read.body.as_bytes() != bodies[ordinal] {
            return Err(fail("imported content differs"));
        }
        let request = ContextRequest {
            scope: ContextScope::IndexedEvidence,
            documents: QueryPlan {
                filters: SearchFilters {
                    source_ids: vec![source.clone()],
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        };
        let context = verification::context(
            &catalog,
            None,
            &format!("importbatchprobe{ordinal:03}"),
            &request,
        )?;
        let passage=context.passages.iter().find(|passage|passage.citations.iter().any(|citation|matches!(citation,CitationRef::Source(reference) if reference.source_id==source&&reference.source_revision==revision))).ok_or_else(||fail("cited readiness omitted imported input"))?;
        let reference = passage
            .citations
            .iter()
            .find_map(|citation| match citation {
                CitationRef::Source(reference)
                    if reference.source_id == source && reference.source_revision == revision =>
                {
                    Some(reference)
                }
                _ => None,
            })
            .unwrap();
        let quote = bodies
            .get(ordinal)
            .and_then(|body| {
                body.get(reference.span.start() as usize..reference.span.end() as usize)
            })
            .ok_or_else(|| fail("readiness span outside original input"))?;
        if Blake3Hash::digest(quote) != reference.quote_hash
            || quote != passage.text.as_bytes()
            || context.network_used
        {
            return Err(fail("cited readiness quote/network differs"));
        }
        ready.push(json!({"ordinal":ordinal,"read":read,"passage":passage,"citation":reference}));
    }
    if ready.len() != 4 {
        return Err(fail("readiness requires all four distinct inputs"));
    }
    Ok(json!(ready))
}

#[test]
#[ignore = "root-supervised release/native pin; exactly8 diagnostic captures; no seeds"]
fn profile_import_group_one_versus_four() {
    if let Err(error) = run_comparison() {
        if let Some(destination) = env::var_os("LWIKI_IMPORT_EXPERIMENT_DIR") {
            let path = PathBuf::from(destination).join("failure.json");
            if path.parent().is_some_and(Path::is_dir) && !path.exists() {
                let _ = write_json(
                    &path,
                    &json!({"status":"error","error":error.to_string(),"retry_allowed":false,"limits":"stop this frozen attempt; preserve all arm state; captures already spent remain spent"}),
                );
            }
        }
        panic!("frozen paired attempt failed: {error}");
    }
}
fn run_comparison() -> Result<()> {
    let destination = requested_directory("LWIKI_IMPORT_EXPERIMENT_DIR")?;
    let manifest = PathBuf::from(
        env::var_os("LWIKI_IMPORT_BASELINE_MANIFEST")
            .ok_or_else(|| fail("existing baseline manifest required"))?,
    );
    let envelope: Value = serde_json::from_slice(&fs::read(&manifest)?)?;
    if envelope["kind"] != "disposable-source-add-sync-clone-pair" {
        return Err(fail("baseline manifest kind differs"));
    }
    let original = PathBuf::from(
        envelope["original_export"]
            .as_str()
            .ok_or_else(|| fail("original baseline path missing"))?,
    );
    if !original.is_absolute()
        || destination.starts_with(&original)
        || original.starts_with(&destination)
    {
        return Err(fail(
            "baseline/artifact paths must be absolute and disjoint",
        ));
    }
    let build_pin_path = PathBuf::from(
        env::var_os("LWIKI_IMPORT_BUILD_PIN")
            .ok_or_else(|| fail("supervisor build pin required"))?,
    );
    let build_pin: Value = serde_json::from_slice(&fs::read(&build_pin_path)?)?;
    if build_pin["profile"] != "release"
        || build_pin["rustc_opt_level"] != 3
        || build_pin["native"] != true
        || build_pin["unit_sha256"].as_str().is_none()
        || build_pin["cli_sha256"].as_str().is_none()
    {
        return Err(fail("native release compiler/executable pin required"));
    }
    let unit_path = env::current_exe()?;
    let cli_path = PathBuf::from(
        build_pin["cli_path"]
            .as_str()
            .ok_or_else(|| fail("CLI executable path required"))?,
    );
    let executable_sha = sha256(&[unit_path.clone(), cli_path.clone()])?;
    if executable_sha[unit_path
        .to_str()
        .ok_or_else(|| fail("unit path nonUTF8"))?]
        != build_pin["unit_sha256"]
        || executable_sha[cli_path.to_str().ok_or_else(|| fail("CLI path nonUTF8"))?]
            != build_pin["cli_sha256"]
    {
        return Err(fail(
            "actual unit/CLI executable SHA differs from supervisor pin",
        ));
    }
    let source_before = inventory(&original)?;
    for entry in envelope["files"]
        .as_array()
        .ok_or_else(|| fail("baseline file commitments missing"))?
    {
        let name = PathBuf::from(
            entry["path"]
                .as_str()
                .ok_or_else(|| fail("baseline entry path missing"))?,
        );
        let actual = source_before
            .get(&name)
            .ok_or_else(|| fail("baseline committed file missing"))?;
        if entry["bytes"] != json!(actual.bytes) || entry["blake3"] != json!(actual.blake3) {
            return Err(fail("existing baseline bytes differ from immutable export"));
        }
    }
    if envelope["files"].as_array().unwrap().len() != source_before.len() {
        return Err(fail("uncommitted baseline files present"));
    }
    let sqlite_paths: Vec<_> = source_before
        .keys()
        .filter(|path| path.to_string_lossy().contains(".sqlite"))
        .map(|path| original.join(path))
        .collect();
    let original_sqlite_sha_before = sha256(&sqlite_paths)?;
    let baseline = destination.join("immutable-baseline-copy");
    clone_tree(&original, &baseline)?;
    let baseline_counts = counts_and_snapshot(&baseline)?;
    let baseline_pins = inventory(&baseline)?;
    let sqlite_relative: Vec<_> = baseline_pins
        .keys()
        .filter(|path| path.to_string_lossy().contains(".sqlite"))
        .cloned()
        .collect();
    let baseline_sqlite_sha_before = sha256(
        &sqlite_relative
            .iter()
            .map(|path| baseline.join(path))
            .collect::<Vec<_>>(),
    )?;
    let arms = [destination.join("group1"), destination.join("group4")];
    for arm in &arms {
        clone_tree(&baseline, arm)?;
        if serde_json::to_value(inventory(arm)?)? != serde_json::to_value(&baseline_pins)? {
            return Err(fail("paired baseline file/time pins differ"));
        }
    }
    let input_root = destination.join("inputs");
    fs::create_dir(&input_root)?;
    let list = input_root.join("inputs.jsonl");
    let mut list_file = File::create(&list)?;
    let mut input_paths = vec![];
    let mut bodies = vec![];
    for ordinal in 0..4 {
        let path = input_root.join(format!("input-{ordinal}.txt"));
        let mut body = format!(
            "importbatchprobe{ordinal:03} vessel stores {} amber tokens. café 東京 🦀.\n",
            17 + ordinal
        )
        .into_bytes();
        body.resize(1024, b'.');
        fs::write(&path, &body)?;
        serde_json::to_writer(
            &mut list_file,
            &json!({"path":path,"title":format!("Measured import {ordinal}"),"media_type":"text/plain"}),
        )?;
        list_file.write_all(b"\n")?;
        input_paths.push(path);
        bodies.push(body);
    }
    drop(list_file);
    let input_sha_before = sha256(&input_paths)?;
    write_json(
        &destination.join("protocol.json"),
        &json!({"version":1,"frozen":true,"baseline_original":original,"baseline_manifest":manifest,"baseline_counts":baseline_counts,"build_pin":build_pin,"actual_executable_sha256":executable_sha,"inputs":input_sha_before,"group_sizes":[1,4],"input_count":4,"diagnostic_captures":8,"cumulative_budget":"87→95; no seed captures; optional controls at most4≤99 not run by this helper","coverage":COVERAGE}),
    )?;
    let mut samples = vec![];
    let mut exact_manifest: Option<Vec<u8>> = None;
    for (arm, group_size) in arms.iter().zip([1, 4]) {
        let adapter = Arc::new(CountingNativeIo::default());
        let erased: Arc<dyn DurableIo> = adapter.clone();
        let app = OfflineApp::new(
            VaultFs::with_io(VaultRoot::explicit(arm)?, erased.clone()),
            options(),
        )?;
        if !Arc::ptr_eq(&app.fs().durable_io(), &erased)
            || !Arc::ptr_eq(&app.fs().clone().durable_io(), &erased)
        {
            return Err(fail("counting adapter Arc replaced"));
        }
        let arm_sqlite_paths = sqlite_relative
            .iter()
            .map(|path| arm.join(path))
            .collect::<Vec<_>>();
        let arm_sqlite_before = sha256(&arm_sqlite_paths)?;
        let output = input_root.join(format!("manifest-group{group_size}.jsonl"));
        adapter.begin();
        crate::vault::paths::profile::begin();
        let started = Instant::now();
        let measured = (|| -> Result<Value> {
            let prepared = super::super::prepare_source_import(&list, &output, false)?;
            let bytes = fs::read(&output)?;
            if let Some(expected) = &exact_manifest {
                if &bytes != expected {
                    return Err(fail("paired manifest bytes differ"));
                }
            } else {
                exact_manifest = Some(bytes);
            }
            let outcome =
                app.source_import_run(&output, "paired finite import experiment", group_size, 4)?;
            if !outcome.completed
                || outcome.imported_items != 4
                || outcome.groups_committed != 4 / group_size as u64
            {
                return Err(fail("paired import did not acknowledge exactly four items"));
            }
            let results = fs::read(app.fs().root().resolve(&outcome.results_path)?)?;
            let frames: Vec<Value> = results
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .map(serde_json::from_slice)
                .collect::<std::result::Result<_, _>>()?;
            let mut items = vec![];
            let mut change_ids = BTreeSet::new();
            for frame in &frames {
                let result = &frame["event"]["result"];
                change_ids.insert(
                    result["change"]["change_id"]
                        .as_str()
                        .ok_or_else(|| fail("group Change ID missing"))?
                        .to_owned(),
                );
                items.extend(
                    result["items"]
                        .as_array()
                        .ok_or_else(|| fail("group items missing"))?
                        .iter()
                        .cloned(),
                );
            }
            if change_ids.len() != 4 / group_size
                || frames.len() != change_ids.len()
                || items
                    .iter()
                    .filter_map(|item| item["ordinal"].as_u64())
                    .collect::<BTreeSet<_>>()
                    != BTreeSet::from([0, 1, 2, 3])
            {
                return Err(fail("group frames/Changes/ordinals differ"));
            }
            let readiness = cited_readiness(&app, &items, &bodies)?;
            Ok(
                json!({"preparation":prepared,"outcome":outcome,"results_bytes":results.len(),"frames":frames,"change_ids":change_ids,"readiness":readiness}),
            )
        })();
        let elapsed_ns = started.elapsed().as_nanos();
        let paths = crate::vault::paths::profile::finish();
        let calls = adapter.finish();
        let total_calls: u64 = calls.values().map(|entry| entry.calls).sum();
        let total_errors: u64 = calls.values().map(|entry| entry.errors).sum();
        let mut sample = json!({"group_size":group_size,"elapsed_ns":elapsed_ns,"adapter_arc_verified":true,"total_adapter_calls":total_calls,"total_adapter_errors":total_errors,"calls":calls,"paths":paths,"coverage":COVERAGE});
        match measured {
            Ok(value) => sample["measurement"] = value,
            Err(error) => {
                sample["error"] = json!(error.to_string());
                samples.push(sample);
                write_json(
                    &destination.join("partial-comparison.json"),
                    &json!({"samples":samples,"status":"error","no_retry":true,"coverage":COVERAGE}),
                )?;
                return Err(error);
            }
        }
        drop(app);
        sample["counts_after"] = counts_and_snapshot(arm)?;
        let expected_groups = 4 / group_size as u64;
        if sample["counts_after"]["sources"].as_u64()
            != baseline_counts["sources"].as_u64().map(|value| value + 4)
            || sample["counts_after"]["changes"].as_u64()
                != baseline_counts["changes"]
                    .as_u64()
                    .map(|value| value + expected_groups)
            || sample["counts_after"]["snapshot"]["generation"].as_u64()
                != baseline_counts["snapshot"]["generation"]
                    .as_u64()
                    .map(|value| value + expected_groups)
        {
            return Err(fail(
                "publication/Source/retained Change count differs from one per group",
            ));
        }
        sample["publications"] = json!(expected_groups);
        sample["retained_changes_added"] = json!(expected_groups);
        sample["sqlite_sha_before"] = arm_sqlite_before;
        sample["sqlite_sha_after"] = sha256(&arm_sqlite_paths)?;
        write_json(
            &destination.join(format!("group{group_size}-sample.json")),
            &sample,
        )?;
        samples.push(sample);
    }
    let baseline_sqlite_sha_after = sha256(
        &sqlite_relative
            .iter()
            .map(|path| baseline.join(path))
            .collect::<Vec<_>>(),
    )?;
    if serde_json::to_value(inventory(&baseline)?)? != serde_json::to_value(&baseline_pins)?
        || baseline_sqlite_sha_before != baseline_sqlite_sha_after
    {
        return Err(fail("immutable baseline copy changed during paired arms"));
    }
    let source_after = inventory(&original)?;
    if serde_json::to_value(&source_before)? != serde_json::to_value(&source_after)? {
        return Err(fail("original immutable baseline changed"));
    }
    let original_sqlite_sha_after = sha256(&sqlite_paths)?;
    let input_sha_after = sha256(&input_paths)?;
    if original_sqlite_sha_before != original_sqlite_sha_after
        || input_sha_before != input_sha_after
    {
        return Err(fail("baseline SQLite or shared input SHA pins changed"));
    }
    write_json(
        &destination.join("comparison.json"),
        &json!({"version":1,"kind":"finite-import-group-count-comparison","baseline_manifest":manifest,"baseline_original":original,"baseline_counts":baseline_counts,"baseline_file_time_pins":baseline_pins,"source_before":source_before,"source_after":source_after,"sqlite_sha_before":original_sqlite_sha_before,"sqlite_sha_after":original_sqlite_sha_after,"input_sha_before":input_sha_before,"input_sha_after":input_sha_after,"build_pin":build_pin,"actual_executable_sha256":executable_sha,"baseline_sqlite_sha_before":baseline_sqlite_sha_before,"baseline_sqlite_sha_after":baseline_sqlite_sha_after,"diagnostic_captures":8,"cumulative_diagnostic_budget":"87→95; no more than4 optional controls≤99; no seeds","samples":samples,"coverage":COVERAGE,"scope":"single paired operation-count diagnostic, no latency speedup, total fsync,25k/~2.5GB,100k future or power-loss qualification"}),
    )?;
    Ok(())
}

#[test]
fn import_count_adapter_forwards_private_creation_bytes_sync_and_native_errors_once() {
    let temp = tempfile::tempdir().unwrap();
    let adapter = Arc::new(CountingNativeIo::default());
    let erased: Arc<dyn DurableIo> = adapter.clone();
    let cloned = erased.clone();
    assert!(Arc::ptr_eq(&erased, &cloned));
    adapter.begin();
    let directory = temp.path().join("private");
    cloned.create_private_directory(&directory).unwrap();
    let path = directory.join("bytes");
    let mut file = cloned.create_private_stage(&path).unwrap();
    cloned
        .write_stage(&mut file, b"exact diagnostic bytes")
        .unwrap();
    cloned.sync_file(&file).unwrap();
    cloned.sync_directory(&directory).unwrap();
    let missing = directory.join("missing");
    let expected = NativeIo.sync_directory(&missing).unwrap_err();
    let actual = cloned.sync_directory(&missing).unwrap_err();
    assert_eq!(actual.raw_os_error(), expected.raw_os_error());
    assert_eq!(actual.kind(), expected.kind());
    let calls = adapter.finish();
    assert_eq!(calls["create_private_stage"].calls, 1);
    assert_eq!(calls["write_stage"].calls, 1);
    assert_eq!(calls["sync_file"].completed, 1);
    assert_eq!(calls["sync_directory"].calls, 2);
    assert_eq!(calls["sync_directory"].errors, 1);
    assert_eq!(fs::read(path).unwrap(), b"exact diagnostic bytes");
}

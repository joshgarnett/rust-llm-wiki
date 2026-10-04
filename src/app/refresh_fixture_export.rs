//! Ignored, bounded fixture export for actual CLI refresh experiments.
//! Capture formatting and normalized publication are real; this is not an import.
use crate::{
    catalog::{
        file_types::{BuildIdentity, CatalogSelection},
        normalized_build::{BuildLimits, NormalizedBuilder},
        normalized_read, scan, selector,
    },
    domain::{Blake3Hash, RecordId, VaultRelativePath},
    sources::{CaptureRequest, ExtractionInput, SourceOrigin, SourceStore},
    vault::{VaultFs, VaultRoot, WriterPermit},
};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use serde_json::json;
use std::{
    env,
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

pub(super) type ExportResult<T> = Result<T, Box<dyn std::error::Error>>;
const VAULT_ID: &str = "vault_refresh_fixture";

fn invalid(message: &str) -> Box<dyn std::error::Error> {
    io::Error::new(io::ErrorKind::InvalidInput, message).into()
}
struct Config {
    output: PathBuf,
    count: usize,
    seed: u64,
    bytes: usize,
}
impl Config {
    fn from_env() -> ExportResult<Self> {
        let output = env::var_os("LWIKI_FIXTURE_EXPORT")
            .map(PathBuf::from)
            .ok_or_else(|| invalid("LWIKI_FIXTURE_EXPORT must name a new absolute directory"))?;
        let count = env::var("LWIKI_FIXTURE_SOURCE_COUNT")?.parse()?;
        let seed = optional_env("LWIKI_FIXTURE_SEED", "731")?.parse()?;
        let bytes = optional_env("LWIKI_FIXTURE_BYTES_PER_SOURCE", "100000")?.parse()?;
        Self::checked(output, count, seed, bytes)
    }
    fn checked(output: PathBuf, count: usize, seed: u64, bytes: usize) -> ExportResult<Self> {
        if !(1..=10000).contains(&count) || !(1024..=1024 * 1024).contains(&bytes) {
            return Err(invalid(
                "fixture requires 1..10000 sources and 1KiB..1MiB per source",
            ));
        }
        require_new_output(&output)?;
        Ok(Self {
            output,
            count,
            seed,
            bytes,
        })
    }
}

fn optional_env(name: &str, default: &str) -> ExportResult<String> {
    match env::var(name) {
        Ok(value) => Ok(value),
        Err(env::VarError::NotPresent) => Ok(default.into()),
        Err(error) => Err(error.into()),
    }
}

/// Reject traversal and symlink ancestors before allocating the new export.
/// Its parent must already exist; this test never creates an arbitrary hierarchy.
fn require_new_output(output: &Path) -> ExportResult<()> {
    if !output.is_absolute()
        || output.to_str().is_none()
        || output
            .components()
            .any(|c| matches!(c, Component::CurDir | Component::ParentDir))
    {
        return Err(invalid("export must be a normalized absolute UTF8 path"));
    }
    match fs::symlink_metadata(output) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
        Ok(_) => return Err(invalid("export already exists; no overwrite is allowed")),
    }
    let parent = output
        .parent()
        .ok_or_else(|| invalid("export has no parent"))?;
    for component in output.components() {
        if let Component::Normal(name) = component {
            VaultRelativePath::new(
                name.to_str()
                    .ok_or_else(|| invalid("nonUTF8 export path"))?,
            )?;
        }
    }
    let mut ancestor = PathBuf::new();
    for component in parent.components() {
        ancestor.push(component);
        let metadata = fs::symlink_metadata(&ancestor)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(invalid(
                "export ancestor must be an existing directory without symlinks",
            ));
        }
    }
    Ok(())
}

fn marker(index: usize) -> String {
    format!("refreshprobe{index:06}v000000")
}
fn payload(seed: u64, index: usize, bytes: usize) -> Vec<u8> {
    let mut result = format!(
        "# Synthetic capture\n\n{}: café 東京 🦀 field observation.\n",
        marker(index)
    )
    .into_bytes();
    // SplitMix64 yields deterministic varied ASCII words, rather than padding.
    let mut state = seed ^ (index as u64).wrapping_mul(0xd6e8feb86659fd93);
    while result.len() < bytes {
        state = state.wrapping_add(0x9e3779b97f4a7c15);
        let mut word = state;
        word = (word ^ (word >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        word = (word ^ (word >> 27)).wrapping_mul(0x94d049bb133111eb);
        word ^= word >> 31;
        result.extend(format!("observation{word:016x} ").as_bytes());
        if result.len() % 8 == 0 {
            result.push(b'\n');
        }
    }
    // All admitted sizes exceed the complete Unicode header, so the cut is ASCII.
    result.truncate(bytes);
    result
}

pub(super) fn create_file(path: &Path, bytes: &[u8]) -> ExportResult<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    Ok(())
}
pub(super) fn create_owned_parents(
    root: &Path,
    relative: &VaultRelativePath,
) -> ExportResult<PathBuf> {
    let mut path = root.to_path_buf();
    let mut components = relative.as_str().split('/').peekable();
    while let Some(component) = components.next() {
        path.push(component);
        if components.peek().is_none() {
            break;
        }
        match fs::create_dir(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(invalid("fixture parent is not an owned directory"));
        }
    }
    Ok(path)
}

pub(super) fn stream_hash(path: &Path, reject_hardlinks: bool) -> ExportResult<(u64, Blake3Hash)> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let mut file = options.open(path)?;
    let before = file.metadata()?;
    if !before.is_file() {
        return Err(invalid("fixture inventory requires regular files"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if reject_hardlinks && before.nlink() != 1 {
            return Err(invalid("fixture inventory refuses hardlinked files"));
        }
    }
    #[cfg(not(unix))]
    let _ = reject_hardlinks;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut bytes = 0u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(count as u64)
            .ok_or_else(|| invalid("inventory byte overflow"))?;
        hasher.update(&buffer[..count]);
    }
    let after = file.metadata()?;
    if bytes != before.len()
        || after.len() != before.len()
        || after.modified()? != before.modified()?
    {
        return Err(invalid("file changed while exporting inventory"));
    }
    Ok((
        bytes,
        Blake3Hash::new(format!("blake3:{}", hasher.finalize()))?,
    ))
}

#[derive(Serialize)]
struct SourceEntry {
    index: usize,
    source_id: RecordId,
    revision_id: RecordId,
    title: String,
    bytes: usize,
    blake3: Blake3Hash,
    marker: String,
}
#[derive(Serialize)]
struct FileEntry {
    path: String,
    bytes: u64,
    blake3: Blake3Hash,
}
fn inventory(root: &Path) -> ExportResult<Vec<FileEntry>> {
    fn visit(root: &Path, directory: &Path, files: &mut Vec<FileEntry>) -> ExportResult<()> {
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Err(invalid("fixture contains a symlink"));
            }
            if metadata.is_dir() {
                visit(root, &path, files)?;
            } else if metadata.is_file() {
                let relative = path
                    .strip_prefix(root)?
                    .components()
                    .map(|c| {
                        c.as_os_str()
                            .to_str()
                            .ok_or_else(|| invalid("nonUTF8 fixture path"))
                    })
                    .collect::<ExportResult<Vec<_>>>()?
                    .join("/");
                VaultRelativePath::new(&relative)?;
                let (bytes, blake3) = stream_hash(&path, true)?;
                files.push(FileEntry {
                    path: relative,
                    bytes,
                    blake3,
                });
            } else {
                return Err(invalid("fixture contains a special file"));
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    visit(root, root, &mut files)?;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

fn export(config: Config) -> ExportResult<()> {
    let started = Instant::now();
    // Recheck immediately before create_new directory allocation.
    require_new_output(&config.output)?;
    fs::create_dir(&config.output)?;
    let vault_path = config.output.join("vault");
    fs::create_dir(&vault_path)?;
    create_file(&vault_path.join("WIKI.md"), b"---\nwiki_schema: '1'\nwiki_kind: vault\nwiki_id: vault_refresh_fixture\ntitle: Refresh fixture\n---\n")?;
    let mut sources = Vec::with_capacity(config.count);
    let capture_started = Instant::now();
    let (completed, layouts, capture_seconds, build_seconds) = {
        let handle = VaultFs::new(VaultRoot::explicit(&vault_path)?);
        let writer = WriterPermit::acquire(handle.root(), Duration::ZERO)?;
        let store = SourceStore::new(handle.clone());
        for index in 0..config.count {
            let body = payload(config.seed, index, config.bytes);
            let hash = Blake3Hash::digest(&body);
            let title = format!("Synthetic capture {index:06}");
            let plan = store.plan_capture(CaptureRequest {
                title: title.clone(),
                origin_kind: SourceOrigin::LocalFile,
                origin: format!("fixture-{index:06}.md"),
                original: body,
                extraction: ExtractionInput::Utf8Preserve,
                media_type: None,
            })?;
            for operation in plan
                .draft
                .ok_or_else(|| invalid("capture omitted draft"))?
                .operations
            {
                let path = create_owned_parents(&vault_path, &operation.target)?;
                let bytes = operation
                    .proposed
                    .ok_or_else(|| invalid("capture deleted a file"))?;
                create_file(&path, &bytes)?;
            }
            sources.push(SourceEntry {
                index,
                source_id: plan.source_id,
                revision_id: plan.revision_id,
                title,
                bytes: config.bytes,
                blake3: hash,
                marker: marker(index),
            });
        }
        let capture_seconds = capture_started.elapsed().as_secs_f64();
        let build_started = Instant::now();
        let vault_id = RecordId::new(VAULT_ID)?;
        let selection = CatalogSelection::new(vault_id.clone(), 1)?;
        selector::prepare(&handle, &writer, &selection)?;
        let identity = BuildIdentity {
            selection,
            origin: None,
            vector_cache_lost: false,
            vector_loss_unknown: false,
        };
        let mut builder =
            NormalizedBuilder::begin(&handle, &writer, identity, BuildLimits::default())?;
        let input = scan::scan_input(&handle, &vault_id)?;
        let projection = scan::project_normalized_with_sink(&handle, &input, false, &mut builder)?;
        let completed = builder.finish_normalized(&projection)?;
        selector::publish(
            &handle,
            &writer,
            &completed.identity.selection,
            Duration::ZERO,
        )?;
        let connection = Connection::open_with_flags(
            &completed.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        let layouts: (i64, i64) = connection.query_row(
            "SELECT proof_layout_version,revision_ownership_version FROM catalog_meta WHERE singleton=1", [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if layouts != (2, 1)
            || normalized_read::header(&connection, &completed.identity.selection)?.snapshot
                != completed.snapshot
        {
            return Err(invalid(
                "exported normalized publication has unexpected state",
            ));
        }
        drop(connection);
        (
            completed,
            layouts,
            capture_seconds,
            build_started.elapsed().as_secs_f64(),
        )
        // Every writer/SQLite/source handle is dropped before inventory.
    };
    let inventory_started = Instant::now();
    let files = inventory(&vault_path)?;
    for source in &sources {
        for name in ["original.bin", "content.md"] {
            let expected = format!(
                "sources/{}/revisions/{}/{name}",
                source.source_id, source.revision_id
            );
            let selected = files
                .binary_search_by(|file| file.path.cmp(&expected))
                .ok()
                .map(|index| &files[index])
                .ok_or_else(|| invalid("capture payload missing from closed inventory"))?;
            if selected.bytes != source.bytes as u64 || selected.blake3 != source.blake3 {
                return Err(invalid(
                    "closed capture payload differs from generated content",
                ));
            }
        }
    }
    for suffix in [".sqlite", ".sqlite-wal", ".sqlite-shm"] {
        let expected = format!(
            ".wiki/cache/catalogs/{}{suffix}",
            completed.identity.selection.file_id
        );
        if files
            .binary_search_by(|file| file.path.cmp(&expected))
            .is_err()
        {
            return Err(invalid("closed normalized serving files are incomplete"));
        }
    }
    let total_file_bytes = files
        .iter()
        .try_fold(0u64, |n, file| n.checked_add(file.bytes))
        .ok_or_else(|| invalid("fixture file bytes overflow"))?;
    let (_, test_binary_hash) = stream_hash(&env::current_exe()?, false)?;
    let corpus = json!({
        "version":1,"kind":"normalized-refresh-fixture","seed":config.seed,
        "bytes_per_source":config.bytes,"source_count":config.count,
        "current_content_bytes":config.count * config.bytes,"target_prior_extra_revisions":0,
        "target_current_revision":sources[0].revision_id,"sources":sources,
        "vault_id":VAULT_ID,"snapshot":completed.snapshot,
        "proof_layout_version":layouts.0,"revision_ownership_version":layouts.1,"files":files,
        "generator":{"source_blake3":Blake3Hash::digest(include_bytes!("refresh_fixture_export.rs")),"test_binary_blake3":test_binary_hash}
    });
    create_file(
        &config.output.join("corpus.json"),
        &serde_json::to_vec_pretty(&corpus)?,
    )?;
    let seed_report = json!({
        "version":1,"kind":"normalized-refresh-fixture-export","status":"exported",
        "source_count":config.count,"current_content_bytes":config.count * config.bytes,
        "history_revisions":0,"evidence_fanout":0,"file_count":files.len(),"total_file_bytes":total_file_bytes,
        "capture_seconds":capture_seconds,"build_and_publish_seconds":build_seconds,
        "inventory_and_binary_hash_seconds":inventory_started.elapsed().as_secs_f64(),
        "total_seconds":started.elapsed().as_secs_f64(),"build_stats":completed.stats,
        "interpretation":"Real capture plan formatting and one normalized full build; direct disposable writes bypass managed import/history receipts. External supervisor owns time/RSS/free-space/allocated-disk bounds. No provider calls."
    });
    create_file(
        &config.output.join("seed-report.json"),
        &serde_json::to_vec_pretty(&seed_report)?,
    )?;
    println!(
        "{}",
        json!({"status":"exported","manifest":config.output.join("corpus.json"),"sources":config.count})
    );
    Ok(())
}

#[test]
#[ignore = "requires explicit new fixture output/count and a bounded native supervisor"]
fn export_normalized_refresh_fixture() {
    export(Config::from_env().expect("fixture export configuration"))
        .expect("normalized fixture export");
}

#[test]
fn capture_payload_is_exact_utf8_varied_and_keeps_marker_near_start() {
    for size in [1024, 100000, 1024 * 1024] {
        let body = payload(731, 0, size);
        assert_eq!(body.len(), size);
        let text = std::str::from_utf8(&body).unwrap();
        assert!(text[..128].contains("refreshprobe000000v000000"));
        assert!(text[..128].contains("東京"));
        assert_eq!(body, payload(731, 0, size));
        assert_ne!(
            Blake3Hash::digest(&body),
            Blake3Hash::digest(payload(731, 1, size))
        );
        assert_ne!(
            Blake3Hash::digest(&body),
            Blake3Hash::digest(payload(732, 0, size))
        );
    }
}

#[test]
fn exporter_refuses_existing_outputs_and_out_of_budget_configuration() {
    let temp = tempfile::tempdir().unwrap();
    let parent = temp.path().canonicalize().unwrap();
    assert!(Config::checked(parent.clone(), 1, 731, 1024).is_err());
    let new = parent.join("new fixture");
    for (count, bytes) in [(0, 1024), (10001, 1024), (1, 1023), (1, 1024 * 1024 + 1)] {
        assert!(Config::checked(new.clone(), count, 731, bytes).is_err());
    }
    assert!(!new.exists());
    assert!(require_new_output(&PathBuf::from("relative/fixture")).is_err());
    #[cfg(unix)]
    {
        let alias = parent.join("alias");
        std::os::unix::fs::symlink(&parent, &alias).unwrap();
        assert!(require_new_output(&alias.join("new")).is_err());
        fs::remove_file(&alias).unwrap();
        let regular = parent.join("regular");
        fs::write(&regular, b"data").unwrap();
        fs::hard_link(&regular, parent.join("hardlink")).unwrap();
        assert!(stream_hash(&regular, true).is_err());
        assert!(inventory(&parent).is_err());
    }
}

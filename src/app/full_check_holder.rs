//! Test-only long-lived lease/storage probe, not a production query-lifetime test.
use crate::{
    catalog::{
        Catalog,
        query_types::{QueryCatalog, QueryReadLimits},
    },
    domain::{Blake3Hash, RecordKind, VaultRelativePath},
    records::parse_note,
    vault::{VaultFs, VaultRoot},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    env,
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

type HolderResult<T> = Result<T, Box<dyn std::error::Error>>;
const FILE_LIMIT: usize = 4096;
const HOLD_LIMIT: Duration = Duration::from_secs(2 * 60 * 60);

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ownership {
    version: u32,
    kind: String,
    vault_path: PathBuf,
    seed_manifest_sha256: String,
    overlay_manifest_sha256: String,
    wiki_blake3: Blake3Hash,
    source_count: usize,
    original_seed_count: usize,
    subset_rehearsal: bool,
    captured_path: VaultRelativePath,
    captured_blake3: Blake3Hash,
    authored_path: VaultRelativePath,
    authored_blake3: Blake3Hash,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Release {
    version: u32,
    kind: String,
    file_id: String,
}

fn canonical_directory(path: &Path) -> io::Result<()> {
    if !path.is_absolute()
        || path.to_str().is_none()
        || path
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
        || fs::canonicalize(path)?.as_os_str() != path.as_os_str()
    {
        return Err(invalid("holder requires canonical absolute UTF8 paths"));
    }
    let mut at = PathBuf::new();
    for part in path.components() {
        at.push(part);
        let metadata = fs::symlink_metadata(&at)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(invalid(
                "holder directory path contains a symlink or non-directory",
            ));
        }
    }
    Ok(())
}

fn read_bounded(path: &Path) -> io::Result<Vec<u8>> {
    let inspected = fs::symlink_metadata(path)?;
    if !inspected.is_file() || inspected.file_type().is_symlink() {
        return Err(invalid("holder input must be a regular non-symlink file"));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let mut file = options.open(path)?;
    let before = file.metadata()?;
    if !before.is_file() || before.len() > FILE_LIMIT as u64 {
        return Err(invalid("holder input exceeds 4096 bytes or is not regular"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.nlink() != 1 || before.dev() != inspected.dev() || before.ino() != inspected.ino()
        {
            return Err(invalid(
                "holder input is hardlinked or changed path binding",
            ));
        }
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(FILE_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    if bytes.len() > FILE_LIMIT
        || bytes.len() as u64 != before.len()
        || after.len() != before.len()
        || after.modified()? != before.modified()?
    {
        return Err(invalid("holder input changed during bounded read"));
    }
    Ok(bytes)
}

fn create_report(path: &Path, report: Value) -> HolderResult<()> {
    let bytes = serde_json::to_vec(&report)?;
    if bytes.len() > FILE_LIMIT {
        return Err(invalid("holder report exceeds 4096 bytes").into());
    }
    let parent = path
        .parent()
        .ok_or_else(|| invalid("holder report has no owned parent"))?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(&bytes)?;
    staged.as_file().sync_all()?;
    // Publish complete JSON atomically and refuse an existing final path.
    // PersistError owns the staging file; dropping it cleans up after failure.
    staged.persist_noclobber(path).map_err(|error| {
        io::Error::new(
            error.error.kind(),
            format!(
                "holder report publication failed at {}: {error}",
                path.display()
            ),
        )
    })?;
    Ok(())
}

#[test]
#[ignore = "requires an explicitly owned full-check benchmark copy and supervisor release"]
fn hold_full_check_snapshot() -> HolderResult<()> {
    let work = env::var_os("LWIKI_FULL_CHECK_HOLDER")
        .map(PathBuf::from)
        .ok_or_else(|| invalid("LWIKI_FULL_CHECK_HOLDER must name an owned work directory"))?;
    canonical_directory(&work)?;
    let vault = work.join("vault");
    canonical_directory(&vault)?;
    let owned: Ownership = serde_json::from_slice(&read_bounded(&work.join("ownership.json"))?)?;
    let valid_sha256 =
        |hash: &str| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit());
    let captured_parts: Vec<_> = owned.captured_path.as_str().split('/').collect();
    if owned.version != 1
        || owned.kind != "full-check-owned-copy"
        || owned.vault_path != vault
        || !matches!(
            (
                owned.source_count,
                owned.original_seed_count,
                owned.subset_rehearsal
            ),
            (2, 1000, true) | (1000, 1000, false) | (10000, 10000, false)
        )
        || !valid_sha256(&owned.seed_manifest_sha256)
        || !valid_sha256(&owned.overlay_manifest_sha256)
        || owned.authored_path.as_str() != "pages/general/000.md"
        || captured_parts.len() != 5
        || captured_parts[0] != "sources"
        || captured_parts[2] != "revisions"
        || captured_parts[4] != "content.md"
    {
        return Err(
            invalid("holder ownership does not match the frozen owned-copy protocol").into(),
        );
    }
    let wiki_bytes = read_bounded(&vault.join("WIKI.md"))?;
    if Blake3Hash::digest(&wiki_bytes) != owned.wiki_blake3 {
        return Err(invalid("holder WIKI hash differs from ownership").into());
    }
    let parsed = parse_note(&wiki_bytes);
    let record = parsed
        .canonical
        .as_ref()
        .filter(|record| record.kind() == RecordKind::Vault)
        .ok_or_else(|| invalid("holder WIKI is not a valid vault record"))?;
    let catalog = Catalog::new(
        VaultFs::new(VaultRoot::explicit(&vault)?),
        record.id().clone(),
    );
    let held = catalog.cached_query_snapshot(QueryReadLimits::default())?;
    let snapshot = held.snapshot().clone();
    let file_id = snapshot
        .publication()
        .filter(|_| held.normalized_layout())
        .ok_or_else(|| invalid("holder requires an actual normalized published snapshot"))?
        .file_id
        .clone();
    let paths = [
        (
            VaultRelativePath::new("WIKI.md")?,
            owned.wiki_blake3.clone(),
        ),
        (owned.captured_path, owned.captured_blake3),
        (owned.authored_path, owned.authored_blake3),
    ];
    let mut documents = Vec::new();
    for (path, hash) in &paths {
        let row = held
            .document(path)?
            .ok_or_else(|| invalid("holder snapshot is missing an exact probe document"))?;
        if &row.hash != hash {
            return Err(invalid("holder probe document hash differs from ownership").into());
        }
        documents.push(row);
    }
    if documents[0].raw_text.as_bytes() != wiki_bytes.as_slice()
        || documents[0].body.as_bytes() != parsed.body()
    {
        return Err(invalid("holder cached WIKI bytes differ from owned WIKI").into());
    }
    let probes: Vec<_> = paths
        .iter()
        .map(|(path, hash)| json!({"path":path,"hash":hash}))
        .collect();
    create_report(
        &work.join("holder-ready.json"),
        json!({"version":1,"kind":"full-check-held-snapshot","pid":std::process::id(),
            "snapshot":snapshot,"wiki_hash":owned.wiki_blake3,"probes":probes,
            "release_probe_budget_rearmed":true}),
    )?;
    let started = Instant::now();
    loop {
        if started.elapsed() >= HOLD_LIMIT {
            return Err(
                io::Error::new(io::ErrorKind::TimedOut, "holder release exceeded 2 hours").into(),
            );
        }
        match read_bounded(&work.join("holder-release.json")) {
            Ok(bytes) => {
                let release: Release = serde_json::from_slice(&bytes)?;
                if release.version != 1
                    || release.kind != "release-full-check-holder"
                    || release.file_id != file_id
                {
                    return Err(invalid("holder release does not name the held publication").into());
                }
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        thread::sleep(Duration::from_millis(100));
    }
    // The original finite handler remains installed throughout idle. Re-arm only
    // the test's final three lookups: this tests lease/storage preservation, not
    // production query lifetimes beyond their unchanged 30-second budget.
    let limits = QueryReadLimits::default();
    let final_started = Instant::now();
    let mut remaining = limits.max_vm_steps;
    held.connection().progress_handler(
        1,
        Some(move || {
            if final_started.elapsed() >= Duration::from_millis(limits.max_elapsed_ms)
                || remaining <= 1
            {
                return true;
            }
            remaining -= 1;
            false
        }),
    )?;
    if held.snapshot() != &snapshot {
        return Err(invalid("held snapshot identity changed").into());
    }
    for original in &documents {
        if held.document(&original.path)?.as_ref() != Some(original) {
            return Err(invalid("held probe document changed during public operations").into());
        }
    }
    create_report(
        &work.join("holder-result.json"),
        json!({"version":1,"status":"released","snapshot":snapshot,
            "wiki_hash":owned.wiki_blake3,"probes":probes,"release_probe_budget_rearmed":true}),
    )?;
    Ok(())
}

//! Fixed ignored diagnostic; root supplies frozen inputs and owned clones, never originals.
use super::source_import_experiment::{CountingNativeIo, counts_and_snapshot};
use super::*;
use crate::sources::import_manifest::{read_manifest_batch, validate_manifest};
use serde::Deserialize;
use std::{env, io, time::Instant};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn require(ok: bool, message: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(io::Error::other(message).into())
    }
}
#[derive(Deserialize, serde::Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Arm {
    id: String,
    vault: PathBuf,
    group_size: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Protocol {
    version: u32,
    arms: Vec<Arm>,
    manifest: PathBuf,
    manifest_hash: Blake3Hash,
    inputs: u64,
    input_bytes: u64,
    report_dir: PathBuf,
    owned_deadline_seconds: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Owned {
    protocol_text: String,
    arms: Vec<Arm>,
}
fn emit(path: &Path, value: &Value) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    NativeIo.sync_directory(
        path.parent()
            .ok_or_else(|| io::Error::other("missing report parent"))?,
    )?;
    Ok(())
}
fn bounded(path: &Path, max: u64) -> Result<Vec<u8>> {
    use std::io::Read;
    let meta = fs::symlink_metadata(path)?;
    require(
        meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= max,
        "bounded regular file required",
    )?;
    let mut bytes = Vec::new();
    File::open(path)?.take(max + 1).read_to_end(&mut bytes)?;
    require(
        bytes.len() as u64 == meta.len(),
        "bounded input length changed",
    )?;
    Ok(bytes)
}
fn arm_run(p: &Protocol, arm: &Arm, report: &mut Value, started: Instant) -> Result<()> {
    require(
        started.elapsed().as_secs() < p.owned_deadline_seconds,
        "owning deadline reached before arm",
    )?;
    let before = counts_and_snapshot(&arm.vault)?;
    report["before"] = before.clone();
    let adapter = Arc::new(CountingNativeIo::default());
    let app = OfflineApp::new(
        VaultFs::with_io(VaultRoot::explicit(&arm.vault)?, adapter.clone()),
        options(),
    )?;
    let key = format!("fixed import history diagnostic {}", arm.id);
    require(
        started.elapsed().as_secs() < 600,
        "owning deadline reached before import",
    )?;
    adapter.begin();
    crate::vault::paths::profile::begin();
    let import_started = Instant::now();
    let outcome = app.source_import_run(&p.manifest, &key, arm.group_size, 256);
    let elapsed = import_started.elapsed().as_nanos();
    let profile = crate::vault::paths::profile::finish();
    let calls = adapter.finish();
    report["import_elapsed_ns"] = json!(elapsed);
    let timing_valid = profile.portable_outer_elapsed_ns <= elapsed;
    report["portable_outer_fraction"] = if timing_valid {
        json!(profile.portable_outer_elapsed_ns as f64 / elapsed.max(1) as f64)
    } else {
        Value::Null
    };
    report["paths"] = serde_json::to_value(profile)?;
    report["calls"] = serde_json::to_value(calls)?;
    match &outcome {
        Ok(value) => report["outcome"] = serde_json::to_value(value)?,
        Err(error) => {
            report["error"] = serde_json::to_value(error)?;
            return Err(error.to_string().into());
        }
    }
    let outcome = outcome?;
    require(
        timing_valid,
        "exclusive portable time exceeds whole import interval",
    )?;
    require(
        started.elapsed().as_secs() < p.owned_deadline_seconds,
        "owning deadline exceeded during import; no later arm",
    )?;
    let status = app.source_import_status(&key)?;
    report["status"] = serde_json::to_value(&status)?;
    let groups = p.inputs / arm.group_size as u64;
    require(
        outcome.completed
            && outcome.imported_items == p.inputs
            && outcome.total_items == p.inputs
            && outcome.groups_committed == groups
            && outcome.manifest_hash == p.manifest_hash,
        "completion/group/manifest commitment differs",
    )?;
    require(
        serde_json::to_value(&outcome)? == serde_json::to_value(status)?,
        "status differs from completed outcome",
    )?;
    let frames = bounded(
        &app.fs().root().resolve(&outcome.results_path)?,
        4 * 1024 * 1024,
    )?;
    let mut ordinals = BTreeSet::new();
    let mut sources = BTreeSet::new();
    let mut revisions = BTreeSet::new();
    let mut changes = BTreeSet::new();
    for line in frames
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
    {
        let frame: Value = serde_json::from_slice(line)?;
        let result = &frame["event"]["result"];
        require(
            frame["event"]["kind"] == "group_committed",
            "unexpected result event",
        )?;
        require(
            changes.insert(
                result["change"]["change_id"]
                    .as_str()
                    .ok_or_else(|| io::Error::other("missing Change"))?
                    .to_owned(),
            ),
            "duplicate Change",
        )?;
        for item in result["items"]
            .as_array()
            .ok_or_else(|| io::Error::other("missing mappings"))?
        {
            let ordinal = item["ordinal"]
                .as_u64()
                .ok_or_else(|| io::Error::other("missing ordinal"))?;
            let source = item["source_id"]
                .as_str()
                .ok_or_else(|| io::Error::other("missing Source"))?;
            let revision = item["revision_id"]
                .as_str()
                .ok_or_else(|| io::Error::other("missing revision"))?;
            RecordId::new(source)?;
            RecordId::new(revision)?;
            require(
                ordinals.insert(ordinal)
                    && sources.insert(source.to_owned())
                    && revisions.insert(revision.to_owned()),
                "duplicate mapping identity",
            )?;
        }
    }
    require(
        ordinals == (0..p.inputs).collect()
            && changes.len() as u64 == groups
            && sources.len() as u64 == p.inputs
            && revisions.len() as u64 == p.inputs,
        "mapping cardinality differs",
    )?;
    report["mappings"] = json!({"ordinals":ordinals.len(),"sources":sources.len(),"revisions":revisions.len(),"changes":changes.len()});
    let after = counts_and_snapshot(&arm.vault)?;
    report["after"] = after.clone();
    require(
        after["sources"].as_u64() == before["sources"].as_u64().map(|n| n + p.inputs)
            && after["changes"].as_u64() == before["changes"].as_u64().map(|n| n + groups)
            && after["snapshot"]["generation"].as_u64()
                == before["snapshot"]["generation"]
                    .as_u64()
                    .map(|n| n + groups),
        "Source/Change/publication delta differs",
    )?;
    report["result"] = json!("complete");
    Ok(())
}
fn owned_root() -> Result<PathBuf> {
    let root = PathBuf::from(
        env::var_os("LWIKI_IMPORT_HISTORY_OWNED_ROOT")
            .ok_or_else(|| io::Error::other("explicit owned experiment root required"))?,
    );
    require(
        root.is_absolute()
            && root
                .file_name()
                .is_some_and(|name| name == "import-history-001")
            && fs::canonicalize(&root)? == root,
        "owned experiment root must be exact absolute canonical import-history-001",
    )?;
    Ok(root)
}

fn run(started: Instant) -> Result<()> {
    let root = owned_root()?;
    let protocol_path = PathBuf::from(
        env::var_os("LWIKI_IMPORT_HISTORY_PROTOCOL")
            .ok_or_else(|| io::Error::other("explicit frozen protocol required"))?,
    );
    require(
        protocol_path.is_absolute()
            && fs::canonicalize(&protocol_path)? == root.join("protocol.json"),
        "protocol outside fixed root",
    )?;
    let raw = bounded(&protocol_path, 65_536)?;
    let p: Protocol = serde_json::from_slice(&raw)?;
    require(
        p.version == 1
            && p.inputs == 256
            && p.input_bytes == 256 * 102400
            && p.owned_deadline_seconds == 600
            && p.arms.len() == 3,
        "fixed experiment limits differ",
    )?;
    require(
        p.report_dir == root.join("output") && fs::canonicalize(&p.report_dir)? == p.report_dir,
        "report directory outside exact owned root",
    )?;
    require(
        fs::read_dir(&p.report_dir)?.next().is_none(),
        "report output must be empty; no rerun",
    )?;
    let owned: Owned =
        serde_json::from_slice(&bounded(&root.join(".import-history-owned.json"), 65_536)?)?;
    require(
        owned.protocol_text.as_bytes() == raw.as_slice() && owned.arms == p.arms,
        "root ownership marker differs from frozen protocol",
    )?;
    for (arm, (id, size)) in p.arms.iter().zip([
        ("small256-group4", 4),
        ("occupied1024-group4", 4),
        ("occupied1024-group8", 8),
    ]) {
        require(
            arm.id == id
                && arm.group_size == size
                && arm.vault == root.join("owned-vaults").join(id)
                && fs::canonicalize(&arm.vault)? == arm.vault,
            "arm ID/path/group differs or aliases original",
        )?;
    }
    let mut samples = p.arms.iter().map(|arm| json!({"id":arm.id,"vault":arm.vault,"group_size":arm.group_size,"result":"UNRUN"})).collect::<Vec<_>>();
    let attempted = (|| -> Result<()> {
        for name in ["OPENAI_API_KEY", "ANTHROPIC_API_KEY"] {
            require(
                env::var_os(name).is_none(),
                "provider key environment must be absent",
            )?;
        }
        require(
            p.manifest.is_absolute()
                && fs::canonicalize(&p.manifest)? == p.manifest
                && p.manifest.starts_with(&root),
            "manifest outside owned root",
        )?;
        let summary = validate_manifest(&p.manifest)?;
        require(
            summary.manifest_hash == p.manifest_hash
                && summary.items == p.inputs
                && summary.input_bytes == p.input_bytes,
            "manifest pins differ",
        )?;
        let mut offset = summary.first_item_offset;
        for ordinal in (0..p.inputs).step_by(8) {
            let batch = read_manifest_batch(&p.manifest, offset, ordinal, 8)?;
            require(
                batch.items.len() == 8 && batch.items.iter().all(|item| item.byte_len == 102400),
                "fixed 100 KiB inputs differ",
            )?;
            offset = batch.next_offset;
        }
        for (arm, sample) in p.arms.iter().zip(&mut samples) {
            let result = arm_run(&p, arm, sample, started);
            if let Err(error) = &result {
                sample["result"] = json!("ERROR");
                sample["diagnostic_error"] = json!(error.to_string());
            }
            sample["owning_elapsed_before_emit_ns"] = json!(started.elapsed().as_nanos());
            emit(&p.report_dir.join(format!("{}.json", arm.id)), sample)?;
            result?;
            require(
                started.elapsed().as_secs() < 600,
                "owning deadline exceeded after emission",
            )?;
        }
        Ok(())
    })();
    emit(
        &p.report_dir.join("summary.json"),
        &json!({"samples":samples,"error":attempted.as_ref().err().map(ToString::to_string),"owning_elapsed_before_final_emit_ns":started.elapsed().as_nanos(),"portable_timing":"portable_outer_fraction uses root exclusive outer portable timing only; raw legacy portable and enumeration timers overlap and must not be added","coverage":"DurableIo calls/bytes only; excludes SQLite and direct filesystem/lock syncs; physical I/O/RSS unavailable; input/original inventories and public cited readiness owned by root","no_retry":true}),
    )?;
    eprintln!(
        "import-history owning elapsed through JSON emission: {} ns",
        started.elapsed().as_nanos()
    );
    attempted
}
#[test]
#[ignore = "explicit frozen LWIKI_IMPORT_HISTORY_PROTOCOL; root-owned clones, three finite arms"]
fn profile_fixed_import_history() {
    let started = Instant::now();
    let result = run(started);
    if let Err(error) = &result {
        if let Ok(root) = owned_root() {
            let output = root.join("output");
            if fs::canonicalize(&output).ok().as_ref() == Some(&output)
                && !output.join("summary.json").exists()
            {
                if let Err(write_error) = emit(
                    &output.join("preflight-error.json"),
                    &json!({"error":error.to_string(),"all_arms":"UNRUN unless per-arm files exist","owning_elapsed_before_emit_ns":started.elapsed().as_nanos(),"no_retry":true}),
                ) {
                    eprintln!("preflight evidence emission failed: {write_error}");
                }
            }
        }
        eprintln!(
            "import-history final failed elapsed: {} ns",
            started.elapsed().as_nanos()
        );
    }
    assert!(
        result.is_ok(),
        "diagnostic failed; owned per-arm evidence emitted where protocol validation allowed: {result:?}"
    );
}

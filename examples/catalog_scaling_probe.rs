//! Bounded example: direct synthetic metadata diagnostics, never import qualification.
//! Usage: generate ABSENT_DIRECTORY SOURCE_COUNT | audit DIRECTORY | validate DIRECTORY | project DIRECTORY
//! Root must enforce process time/RSS/disk limits externally. No deletion is performed.
use lwiki::{
    catalog::{CatalogGraphValidator, CatalogProjection, RecordRow, scan},
    changes::{GraphValidator, ScanDocument, ValidationInput},
    domain::{Blake3Hash, CanonicalRecord, Eligibility, RecordId, RecordKind, VaultRelativePath},
    sources::evidence::exact_quote_body,
    vault::{ExpectedState, VaultFs, VaultRoot},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fs::{self, File},
    io::{self, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    time::Instant,
};

type ProbeResult<T> = Result<T, Box<dyn Error>>;
const SCHEMA: &str = "lwiki.metadata-scaling-fixture.v1";
const VAULT_ID: &str = "vault_scaling_probe";
const CONTENT_BYTES: usize = 100_000;
const PAGES: usize = 1_000;
const FIXED_CONTROLS: usize = 8; // WIKI + three entities + assertion + two evidence + decision
const MAX_SOURCES: usize = 10_000;
const MANIFEST_NAME: &str = "fixture-manifest.json";
const TIMESTAMP: &str = "2026-09-28T00:00:00Z";
const PREFIX: &[u8] = b"# Captured source\nA uses B.\n";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileEntry {
    path: VaultRelativePath,
    hash: Blake3Hash,
    bytes: usize,
    canonical: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureManifest {
    schema: String,
    seed: String,
    sources: usize,
    pages: usize,
    content_bytes_each: usize,
    control_files: usize,
    payload_files: usize,
    total_content_bytes: usize,
    control_manifest: Blake3Hash,
    files: Vec<FileEntry>,
}

fn invalid(message: impl Into<String>) -> Box<dyn Error> {
    io::Error::new(io::ErrorKind::InvalidInput, message.into()).into()
}
fn envelope(kind: &str, name: &str, extra: Value, body: &[u8]) -> ProbeResult<Vec<u8>> {
    let mut fields = BTreeMap::from([
        ("wiki_schema".into(), json!("1")),
        ("wiki_id".into(), json!(name)),
        ("wiki_kind".into(), json!(kind)),
        ("title".into(), json!(name)),
    ]);
    let extra = extra
        .as_object()
        .ok_or_else(|| invalid("envelope fields must be object"))?;
    fields.extend(
        extra
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    CanonicalRecord::new(fields.clone())?;
    let mut bytes = b"---\n".to_vec();
    for (key, value) in fields {
        writeln!(&mut bytes, "{key}: {value}")?;
    }
    bytes.extend_from_slice(b"---\n");
    bytes.extend_from_slice(body);
    Ok(bytes)
}
fn write_fixture(
    root: &Path,
    name: &str,
    bytes: &[u8],
    canonical: bool,
    files: &mut Vec<FileEntry>,
) -> ProbeResult<()> {
    let path = VaultRelativePath::new(name)?;
    let target = root.join(path.as_str());
    fs::create_dir_all(
        target
            .parent()
            .ok_or_else(|| invalid("fixture path has no parent"))?,
    )?;
    let mut file = File::options().write(true).create_new(true).open(target)?;
    file.write_all(bytes)?;
    files.push(FileEntry {
        path,
        hash: Blake3Hash::digest(bytes),
        bytes: bytes.len(),
        canonical,
    });
    Ok(())
}
fn source_payload(ordinal: usize) -> Vec<u8> {
    let mut bytes = PREFIX.to_vec();
    let mut paragraph = 0u64;
    while bytes.len() < CONTENT_BYTES {
        // Fixed arithmetic and paragraph IDs give varied reproducible UTF-8 bytes.
        let word = (ordinal as u64)
            .wrapping_mul(0x9e37_79b9)
            .wrapping_add(paragraph.wrapping_mul(0x85eb_ca6b));
        let line = format!(
            "Source {ordinal:06} paragraph {paragraph:06}: observation {word:016x}; café 日本; component notes and deterministic context.\n"
        );
        if bytes.len() + line.len() > CONTENT_BYTES {
            // A short ASCII trailer fills the exact byte size without splitting UTF-8.
            let remaining = CONTENT_BYTES - bytes.len();
            bytes.extend((0..remaining).map(|i| {
                if i + 1 == remaining {
                    b'\n'
                } else {
                    b'a' + (i % 26) as u8
                }
            }));
        } else {
            bytes.extend_from_slice(line.as_bytes());
        }
        paragraph += 1;
    }
    bytes
}
fn control_digest(root: &Path, files: &[FileEntry]) -> ProbeResult<Blake3Hash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lwiki-canonical-control-v1\0");
    for entry in files.iter().filter(|entry| entry.canonical) {
        let bytes = fs::read(root.join(entry.path.as_str()))?;
        if bytes.len() != entry.bytes || Blake3Hash::digest(&bytes) != entry.hash {
            return Err(invalid("generated control file changed"));
        }
        hasher.update(&(entry.path.as_str().len() as u64).to_le_bytes());
        hasher.update(entry.path.as_str().as_bytes());
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    Ok(Blake3Hash::new(format!(
        "blake3:{}",
        hasher.finalize().to_hex()
    ))?)
}
// Shared deterministic byte recipe. Cheap manifest checks omit payload generation;
// the full audit regenerates and compares every expected control/payload byte.
fn visit_fixture(
    sources: usize,
    include_payloads: bool,
    mut emit: impl FnMut(&str, &[u8], bool) -> ProbeResult<()>,
) -> ProbeResult<()> {
    emit(
        "WIKI.md",
        &envelope(
            "vault",
            VAULT_ID,
            json!({}),
            b"Disposable scaling diagnostic\n",
        )?,
        true,
    )?;
    for ordinal in 0..sources {
        let source = format!("source_{ordinal:06}");
        let revision = format!("revision_{ordinal:06}");
        let payload = if include_payloads {
            source_payload(ordinal)
        } else {
            Vec::new()
        };
        let hash = Blake3Hash::digest(&payload);
        emit(
            &format!("sources/{source}/source.md"),
            &envelope(
                "source",
                &source,
                json!({
                    "wiki_status":"active", "wiki_origin_kind":"local-file", "wiki_origin":"synthetic-metadata-diagnostic",
                    "wiki_current_revision":revision, "wiki_revisions":[revision]
                }),
                b"Synthetic captured source\n",
            )?,
            true,
        )?;
        let directory = format!("sources/{source}/revisions/{revision}");
        emit(
            &format!("{directory}/revision.md"),
            &envelope(
                "revision",
                &revision,
                json!({
                    "wiki_source_id":source, "wiki_captured_at":TIMESTAMP,
                    "wiki_original_path":"original.bin", "wiki_original_hash":hash,
                    "wiki_extractor":"synthetic-utf8-preserve", "wiki_extractor_fingerprint":Blake3Hash::digest(b"scaling-fixture-v1"),
                    "wiki_extraction_status":"complete", "wiki_content_path":"content.md", "wiki_content_hash":hash
                }),
                b"Immutable synthetic revision\n",
            )?,
            true,
        )?;
        emit(&format!("{directory}/original.bin"), &payload, false)?;
        emit(&format!("{directory}/content.md"), &payload, false)?;
    }
    for entity in ["a", "b", "c"] {
        emit(
            &format!("entities/{entity}.md"),
            &envelope(
                "entity",
                entity,
                json!({
                    "wiki_status":"active", "wiki_entity_type":"component"
                }),
                b"Unsupported description; identity remains current.\n",
            )?,
            true,
        )?;
    }
    emit(
        "assertions/use.md",
        &envelope(
            "assertion",
            "use",
            json!({
                "wiki_status":"accepted", "wiki_subject_id":"a", "wiki_subject":"[[entities/a]]",
                "wiki_object_id":"b", "wiki_predicate":"uses"
            }),
            b"A uses B.\n",
        )?,
        true,
    )?;
    let quote = b"A uses B.";
    for (name, stance) in [("support", "supports"), ("opposition", "contradicts")] {
        emit(
            &format!("evidence/{name}.md"),
            &envelope(
                "evidence",
                name,
                json!({
                    "wiki_status":"active", "wiki_assertion_id":"use", "wiki_source_id":"source_000000",
                    "wiki_source_revision":"revision_000000", "wiki_stance":stance, "wiki_locator_kind":"utf8-bytes",
                    "wiki_span_start":18, "wiki_span_end":18 + quote.len(), "wiki_quote_hash":Blake3Hash::digest(quote)
                }),
                &exact_quote_body(quote, "\n", "Synthetic explanation")?,
            )?,
            true,
        )?;
    }
    emit(
        "decisions/accept.md",
        &envelope(
            "decision",
            "accept",
            json!({
                "wiki_status":"active", "wiki_action":"accept", "wiki_input_ids":["use"], "wiki_output_ids":["use"],
                "wiki_created_at":TIMESTAMP
            }),
            b"Explicit acceptance\n",
        )?,
        true,
    )?;
    for ordinal in 0..PAGES {
        let fields = if ordinal < 16 {
            json!({"wiki_status":"reviewed", "wiki_depends_on_ids":["use"]})
        } else {
            json!({"wiki_status":"reviewed"})
        };
        let body =
            format!("# Authored page {ordinal:06}\nSynthetic independently authored note text.\n");
        emit(
            &format!("pages/page_{ordinal:06}.md"),
            &envelope(
                "page",
                &format!("page_{ordinal:06}"),
                fields,
                body.as_bytes(),
            )?,
            true,
        )?;
    }
    Ok(())
}

fn generate(root: &Path, sources: usize) -> ProbeResult<Value> {
    if !(1..=MAX_SOURCES).contains(&sources) {
        return Err(invalid("source count must be 1..10000"));
    }
    // create_dir refuses any existing directory/file/symlink. No cleanup follows errors.
    fs::create_dir(root)?;
    let started = Instant::now();
    let mut files = Vec::new();
    visit_fixture(sources, true, |name, bytes, canonical| {
        write_fixture(root, name, bytes, canonical, &mut files)
    })?;
    files.sort_by(|a, b| a.path.as_str().as_bytes().cmp(b.path.as_str().as_bytes()));
    let control_manifest = control_digest(root, &files)?;
    let manifest = FixtureManifest {
        schema: SCHEMA.into(),
        seed: "lwiki-metadata-diagnostic-v1".into(),
        sources,
        pages: PAGES,
        content_bytes_each: CONTENT_BYTES,
        control_files: 2 * sources + PAGES + FIXED_CONTROLS,
        payload_files: 2 * sources,
        total_content_bytes: sources * CONTENT_BYTES,
        control_manifest,
        files,
    };
    if manifest.files.len() != manifest.control_files + manifest.payload_files {
        return Err(invalid("generated file count differs from design"));
    }
    let mut output = BufWriter::new(
        File::options()
            .write(true)
            .create_new(true)
            .open(root.join(MANIFEST_NAME))?,
    );
    serde_json::to_writer_pretty(&mut output, &manifest)?;
    output.flush()?;
    Ok(
        json!({"mode":"generate", "sources":sources, "control_files":manifest.control_files,
        "payload_files":manifest.payload_files, "total_content_bytes":manifest.total_content_bytes,
        "control_manifest":manifest.control_manifest, "manifest_hash":Blake3Hash::digest(fs::read(root.join(MANIFEST_NAME))?),
        "generation_seconds":started.elapsed().as_secs_f64(), "qualification":"synthetic metadata diagnostic only"}),
    )
}

#[derive(Default)]
struct CountingWriter(usize);
impl Write for CountingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn serialized_bytes(value: &impl Serialize) -> ProbeResult<usize> {
    let mut count = CountingWriter::default();
    serde_json::to_writer(&mut count, value)?;
    Ok(count.0)
}
fn read_manifest(root: &Path) -> ProbeResult<FixtureManifest> {
    let manifest_file = File::open(root.join(MANIFEST_NAME))?;
    if manifest_file.metadata()?.len() > 16 * 1024 * 1024 {
        return Err(invalid("fixture manifest exceeds 16MiB"));
    }
    let manifest: FixtureManifest =
        serde_json::from_reader(BufReader::new(manifest_file.take(16 * 1024 * 1024 + 1)))?;
    if manifest.schema != SCHEMA
        || manifest.seed != "lwiki-metadata-diagnostic-v1"
        || !(1..=MAX_SOURCES).contains(&manifest.sources)
        || manifest.pages != PAGES
        || manifest.content_bytes_each != CONTENT_BYTES
        || manifest.control_files != 2 * manifest.sources + PAGES + FIXED_CONTROLS
        || manifest.payload_files != 2 * manifest.sources
        || manifest.total_content_bytes != manifest.sources * CONTENT_BYTES
        || manifest.files.len() != manifest.control_files + manifest.payload_files
    {
        return Err(invalid("unsupported fixture manifest"));
    }
    let mut expected = BTreeMap::new();
    visit_fixture(manifest.sources, false, |name, bytes, canonical| {
        expected.insert(
            VaultRelativePath::new(name)?,
            (
                canonical,
                if canonical {
                    bytes.len()
                } else {
                    CONTENT_BYTES
                },
            ),
        );
        Ok(())
    })?;
    let mut previous: Option<&str> = None;
    for entry in &manifest.files {
        if previous.is_some_and(|old| old.as_bytes() >= entry.path.as_str().as_bytes()) {
            return Err(invalid("manifest paths must be bytewise sorted and unique"));
        }
        previous = Some(entry.path.as_str());
        if expected.get(&entry.path) != Some(&(entry.canonical, entry.bytes)) {
            return Err(invalid(format!(
                "manifest path/size/canonical role mismatch: {}",
                entry.path
            )));
        }
    }
    if expected.len() != manifest.files.len() {
        return Err(invalid("manifest deterministic membership count mismatch"));
    }
    Ok(manifest)
}

fn load_fixture(root: &Path) -> ProbeResult<(VaultFs, FixtureManifest, ValidationInput)> {
    let manifest = read_manifest(root)?;
    let fs = VaultFs::new(VaultRoot::explicit(root)?);
    let mut documents = Vec::new();
    for path in fs.root().scan_markdown()? {
        let before = fs
            .read_before(&path)?
            .ok_or_else(|| invalid("canonical file disappeared"))?;
        documents.push(ScanDocument {
            path,
            bytes: before.bytes,
            hash: before.hash,
        });
    }
    if documents.len() != manifest.control_files {
        return Err(invalid("canonical scan count mismatch"));
    }
    let input = ValidationInput {
        vault_id: RecordId::new(VAULT_ID)?,
        documents,
        overlay: vec![],
    };
    Ok((fs, manifest, input))
}

#[derive(Default, Serialize)]
struct RoleTotal {
    files: usize,
    logical_bytes: u64,
    allocated_bytes: u64,
}
fn file_role(name: &str) -> &'static str {
    if name == "WIKI.md" {
        "vault"
    } else if name.starts_with("entities/") {
        "entity"
    } else if name.starts_with("assertions/") {
        "assertion"
    } else if name.starts_with("evidence/") {
        "evidence"
    } else if name.starts_with("decisions/") {
        "decision"
    } else if name.starts_with("pages/") {
        "page"
    } else if name.ends_with("/source.md") {
        "source"
    } else if name.ends_with("/revision.md") {
        "revision"
    } else if name.ends_with("/content.md") {
        "content"
    } else {
        "original"
    }
}
#[cfg(unix)]
fn file_allocation(meta: &fs::Metadata) -> ProbeResult<u64> {
    use std::os::unix::fs::MetadataExt;
    if meta.nlink() != 1 {
        return Err(invalid("fixture hardlink rejected"));
    }
    let allocated = meta
        .blocks()
        .checked_mul(512)
        .ok_or_else(|| invalid("allocated byte overflow"))?;
    if allocated < meta.len() {
        return Err(invalid("sparse/compressed fixture allocation rejected"));
    }
    Ok(allocated)
}
#[cfg(not(unix))]
fn file_allocation(_meta: &fs::Metadata) -> ProbeResult<u64> {
    Err(invalid(
        "native hardlink/allocation checks unavailable on this platform",
    ))
}
fn membership(root: &Path, expected: &BTreeSet<String>) -> ProbeResult<u64> {
    let root_meta = fs::symlink_metadata(root)?;
    if !root_meta.is_dir() || root_meta.file_type().is_symlink() {
        return Err(invalid("fixture root must be a real directory"));
    }
    let mut expected_dirs = BTreeSet::new();
    for name in expected {
        let mut parent = Path::new(name).parent();
        while let Some(path) = parent.filter(|path| !path.as_os_str().is_empty()) {
            expected_dirs.insert(path.to_string_lossy().into_owned());
            parent = path.parent();
        }
    }
    let mut found = BTreeSet::new();
    let mut found_dirs = BTreeSet::new();
    let mut pending = vec![root.to_path_buf()];
    let mut allocated = 0u64;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let relative = path
                .strip_prefix(root)?
                .to_str()
                .ok_or_else(|| invalid("non-UTF8 fixture path"))?
                .to_owned();
            let meta = fs::symlink_metadata(&path)?;
            if meta.file_type().is_symlink() {
                return Err(invalid(format!("fixture symlink rejected: {relative}")));
            }
            if meta.is_dir() {
                if !expected_dirs.contains(&relative) {
                    return Err(invalid(format!("unlisted fixture directory: {relative}")));
                }
                found_dirs.insert(relative);
                pending.push(path);
            } else if meta.is_file() {
                if !expected.contains(&relative) {
                    return Err(invalid(format!("unlisted fixture file: {relative}")));
                }
                allocated += file_allocation(&meta)?;
                found.insert(relative);
            } else {
                return Err(invalid("nonregular fixture entry rejected"));
            }
        }
    }
    if found != *expected || found_dirs != expected_dirs {
        return Err(invalid("fixture exact file/directory membership mismatch"));
    }
    Ok(allocated)
}
fn audit(root: &Path) -> ProbeResult<Value> {
    let started = Instant::now();
    // Read metadata before following manifest contents; membership covers all assets.
    let meta = fs::symlink_metadata(root.join(MANIFEST_NAME))?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(invalid("manifest must be regular, not symlink"));
    }
    file_allocation(&meta)?;
    let manifest = read_manifest(root)?;
    let manifest_hash = Blake3Hash::digest(fs::read(root.join(MANIFEST_NAME))?);
    let declared: BTreeMap<_, _> = manifest
        .files
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect();
    let mut expected_files: BTreeSet<_> = declared.keys().map(|name| (*name).to_owned()).collect();
    expected_files.insert(MANIFEST_NAME.into());
    let allocated_files_bytes = membership(root, &expected_files)?;
    let mut roles: BTreeMap<&str, RoleTotal> = BTreeMap::new();
    let mut content_hashes = BTreeSet::new();
    visit_fixture(manifest.sources, true, |name, expected_bytes, canonical| {
        let entry = declared
            .get(name)
            .ok_or_else(|| invalid("missing deterministic manifest member"))?;
        let metadata = fs::symlink_metadata(root.join(name))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(invalid("audit requires regular fixture bytes"));
        }
        let allocation = file_allocation(&metadata)?;
        if metadata.len() != entry.bytes as u64
            || entry.bytes != expected_bytes.len()
            || entry.canonical != canonical
        {
            return Err(invalid(format!("audit role/size mismatch: {name}")));
        }
        let actual = fs::read(root.join(name))?;
        if actual != expected_bytes || Blake3Hash::digest(&actual) != entry.hash {
            return Err(invalid(format!(
                "audit deterministic bytes/hash mismatch: {name}"
            )));
        }
        // Exact deterministic control bytes bind source/current-revision ownership,
        // manifests, original/content hashes, IDs, quote span and quotation bodies.
        let role = file_role(name);
        let total = roles.entry(role).or_default();
        total.files += 1;
        total.logical_bytes += actual.len() as u64;
        total.allocated_bytes += allocation;
        if role == "content" {
            content_hashes.insert(entry.hash.clone());
            if name.starts_with("sources/source_000000/")
                && actual.get(18..27) != Some(b"A uses B.".as_slice())
            {
                return Err(invalid("independent evidence quote span mismatch"));
            }
        }
        Ok(())
    })?;
    if content_hashes.len() != manifest.sources
        || roles["content"].logical_bytes != manifest.total_content_bytes as u64
        || roles["original"].logical_bytes != manifest.total_content_bytes as u64
        || roles["source"].files != manifest.sources
        || roles["revision"].files != manifest.sources
        || roles["page"].files != PAGES
        || roles["decision"].files != 1
    {
        return Err(invalid(
            "audited distinct content hashes/per-role totals mismatch",
        ));
    }
    if control_digest(root, &manifest.files)? != manifest.control_manifest {
        return Err(invalid("audited control digest mismatch"));
    }
    if membership(root, &expected_files)? != allocated_files_bytes
        || Blake3Hash::digest(fs::read(root.join(MANIFEST_NAME))?) != manifest_hash
    {
        return Err(invalid("fixture membership/manifest changed during audit"));
    }
    // This pin binds every declared path/role/size/hash and the manifest itself.
    // Content hashes were independently checked against actual bytes above.
    let mut inventory = blake3::Hasher::new();
    inventory.update(b"lwiki-scaling-audit-inventory-v1\0");
    inventory.update(manifest_hash.as_str().as_bytes());
    for entry in &manifest.files {
        inventory.update(&(entry.path.as_str().len() as u64).to_le_bytes());
        inventory.update(entry.path.as_str().as_bytes());
        inventory.update(&(entry.bytes as u64).to_le_bytes());
        inventory.update(&[u8::from(entry.canonical)]);
        inventory.update(entry.hash.as_str().as_bytes());
    }
    Ok(
        json!({"mode":"audit","sources":manifest.sources,"control_files":manifest.control_files,"payload_files":manifest.payload_files,
        "manifest_placement":"inside fixture root; noncanonical JSON; included in membership/allocated bytes/pins",
        "manifest_hash":manifest_hash,"inventory_hash":format!("blake3:{}",inventory.finalize().to_hex()),"control_manifest":manifest.control_manifest,
        "distinct_content_hashes":content_hashes.len(),"roles":roles,"allocated_regular_file_bytes":allocated_files_bytes,
        "quote":{"source":"source_000000","revision":"revision_000000","start":18,"end":27,"hash":Blake3Hash::digest(b"A uses B.")},
        "audit_seconds":started.elapsed().as_secs_f64(),"allocation_check":"native Unix st_blocks*512 >= file length and nlink=1",
        "qualification":"deterministic synthetic fixture audit only; no concurrent atomic filesystem snapshot claim"}),
    )
}
fn row<'a>(
    projection: &'a CatalogProjection,
    name: &str,
    kind: RecordKind,
) -> ProbeResult<&'a RecordRow> {
    let row = projection
        .records
        .get(&RecordId::new(name)?)
        .ok_or_else(|| invalid(format!("missing expected record: {name}")))?;
    if row.record.kind() != kind {
        return Err(invalid("expected record kind mismatch"));
    }
    Ok(row)
}
fn assert_full_graph(projection: &CatalogProjection, sources: usize) -> ProbeResult<()> {
    for entity in ["a", "b", "c"] {
        let row = row(projection, entity, RecordKind::Entity)?;
        if row.authored_status.as_deref() != Some("active")
            || row.identity_eligibility != Some(Eligibility::Current)
            || row.description_eligibility != Some(Eligibility::Unsupported)
        {
            return Err(invalid("entity identity/description smoke mismatch"));
        }
    }
    let assertion = row(projection, "use", RecordKind::Assertion)?;
    if assertion.authored_status.as_deref() != Some("accepted")
        || assertion.eligibility != Eligibility::Current
        || !assertion.disputed
    {
        return Err(invalid("accepted disputed assertion smoke mismatch"));
    }
    for (name, stance) in [("support", "supports"), ("opposition", "contradicts")] {
        let evidence = row(projection, name, RecordKind::Evidence)?;
        if evidence.eligibility != Eligibility::Current
            || evidence.authored_status.as_deref() != Some("active")
            || evidence.record.string("wiki_stance") != Some(stance)
            || evidence.record.string("wiki_source_id") != Some("source_000000")
            || evidence.record.string("wiki_source_revision") != Some("revision_000000")
            || evidence.record.field("wiki_span_start") != Some(&json!(18))
            || evidence.record.field("wiki_span_end") != Some(&json!(27))
            || evidence.record.string("wiki_quote_hash")
                != Some(Blake3Hash::digest(b"A uses B.").as_str())
        {
            return Err(invalid("current evidence/quote smoke mismatch"));
        }
    }
    let decision = row(projection, "accept", RecordKind::Decision)?;
    if decision.eligibility != Eligibility::Current
        || decision.authored_status.as_deref() != Some("active")
        || decision.record.string("wiki_action") != Some("accept")
    {
        return Err(invalid("active accept decision smoke mismatch"));
    }
    for ordinal in 0..PAGES {
        let page = row(projection, &format!("page_{ordinal:06}"), RecordKind::Page)?;
        if page.eligibility != Eligibility::Current
            || page.authored_status.as_deref() != Some("reviewed")
        {
            return Err(invalid("reviewed page smoke mismatch"));
        }
        let expected = if ordinal < 16 {
            Some(json!(["use"]))
        } else {
            None
        };
        if page.record.field("wiki_depends_on_ids") != expected.as_ref() {
            return Err(invalid("page dependency scope smoke mismatch"));
        }
    }
    for ordinal in 0..sources {
        let source_id = format!("source_{ordinal:06}");
        let revision_id = format!("revision_{ordinal:06}");
        let source = row(projection, &source_id, RecordKind::Source)?;
        let revision = row(projection, &revision_id, RecordKind::Revision)?;
        if source.eligibility != Eligibility::Current
            || source.authored_status.as_deref() != Some("active")
            || source.record.string("wiki_current_revision") != Some(revision_id.as_str())
            || revision.eligibility != Eligibility::Current
            || revision.record.string("wiki_source_id") != Some(source_id.as_str())
            || revision.record.string("wiki_extraction_status") != Some("complete")
        {
            return Err(invalid("source/revision ownership smoke mismatch"));
        }
    }
    if projection.graph.len() != 4 {
        return Err(invalid("fixed graph row count mismatch"));
    }
    Ok(())
}

fn inspect(root: &Path, mode: &str) -> ProbeResult<Value> {
    let loading = Instant::now();
    let (fs, manifest, input) = load_fixture(root)?;
    let loading_seconds = loading.elapsed().as_secs_f64();
    let input_bytes: usize = input
        .documents
        .iter()
        .map(|document| document.bytes.len())
        .sum();
    if mode == "project" && manifest.sources > 1000 {
        return Err(invalid("full projection comparison capped at 1000 sources"));
    }
    let expected: BTreeMap<_, _> = manifest
        .files
        .iter()
        .map(|entry| (entry.path.clone(), ExpectedState::Hash(entry.hash.clone())))
        .collect();
    if expected.len() != manifest.files.len() {
        return Err(invalid("duplicate fixture manifest path"));
    }
    if mode == "validate" {
        let started = Instant::now();
        let result = CatalogGraphValidator.validate(&fs, &input)?;
        let validation_seconds = started.elapsed().as_secs_f64();
        let actual: BTreeMap<_, _> = result
            .dependencies
            .iter()
            .map(|dependency| (dependency.path.clone(), dependency.expected.clone()))
            .collect();
        if actual != expected
            || actual.len() != result.dependencies.len()
            || result.control_manifest != manifest.control_manifest
        {
            return Err(invalid(
                "validator dependencies/control digest differ from generated authority",
            ));
        }
        Ok(
            json!({"mode":mode, "sources":manifest.sources, "canonical_files":input.documents.len(), "input_bytes":input_bytes,
            "input_loading_seconds":loading_seconds, "validation_seconds":validation_seconds,
            "dependencies":result.dependencies.len(), "dependency_serialized_bytes":serialized_bytes(&result.dependencies)?,
            "parser_fingerprint":result.parser_fingerprint, "control_manifest":result.control_manifest,
            "per_record_dependencies":"unavailable through ValidatedGraph", "snapshot_counters":"unavailable through public API",
            "interval":"CatalogGraphValidator.validate return; includes proposed+baseline metadata projections; excludes loading/assertions/serialization",
            "qualification":"synthetic metadata diagnostic only; unchanged invalid baselines remain tolerated by validator"}),
        )
    } else {
        let started = Instant::now();
        let result = scan::project(&fs, &input)?;
        let projection_seconds = started.elapsed().as_secs_f64();
        let actual: BTreeMap<_, _> = result
            .dependencies
            .iter()
            .map(|dependency| (dependency.path.clone(), dependency.expected.clone()))
            .collect();
        if actual != expected
            || actual.len() != result.dependencies.len()
            || result.control_manifest != manifest.control_manifest
            || !result.diagnostics.is_empty()
            || result.records.len() != manifest.control_files
            || result.records[&RecordId::new("use")?].eligibility != Eligibility::Current
            || !result.records[&RecordId::new("use")?].disputed
        {
            return Err(invalid(
                "full projection failed fixture authority/graph expectations",
            ));
        }
        assert_full_graph(&result, manifest.sources)?;
        let mut per_record_counts: Vec<_> = result
            .records
            .values()
            .map(|row| row.dependencies.len())
            .collect();
        per_record_counts.sort_unstable();
        let mut per_record_bytes: Vec<_> = result
            .records
            .values()
            .map(|row| serialized_bytes(&row.dependencies))
            .collect::<ProbeResult<_>>()?;
        per_record_bytes.sort_unstable();
        let sum_counts: usize = per_record_counts.iter().sum();
        let sum_bytes: usize = per_record_bytes.iter().sum();
        let quantile = |values: &[usize], percent: usize| {
            values[(values.len() * percent).div_ceil(100).saturating_sub(1)]
        };
        let source_rows = result
            .documents
            .iter()
            .filter(|row| row.owner_revision.is_some())
            .count();
        if source_rows != manifest.sources {
            return Err(invalid("source retrieval row count mismatch"));
        }
        Ok(
            json!({"mode":mode, "sources":manifest.sources, "canonical_files":input.documents.len(), "input_bytes":input_bytes,
            "input_loading_seconds":loading_seconds, "projection_seconds":projection_seconds, "records":result.records.len(), "source_rows":source_rows,
            "dependencies":result.dependencies.len(), "dependency_serialized_bytes":serialized_bytes(&result.dependencies)?,
            "per_record_dependency_count":{"sum":sum_counts,"p50":quantile(&per_record_counts,50),"p95":quantile(&per_record_counts,95),"max":per_record_counts.last()},
            "per_record_dependency_serialized_bytes":{"sum":sum_bytes,"p50":quantile(&per_record_bytes,50),"p95":quantile(&per_record_bytes,95),"max":per_record_bytes.last()},
            "retained_retrieval_bytes":result.documents.iter().map(|row| row.raw_text.len()+row.body.len()+row.headings.len()).sum::<usize>(),
            "snapshot_counters":"unavailable through public API", "graph_smoke_assertions":"all current sources/revisions; 1000 reviewed pages/16 dependent; 3 current identities/unsupported descriptions; current disputed accepted assertion/current evidence/active decision",
            "qualification":"separate full-text diagnostic; never include in metadata RSS process"}),
        )
    }
}
fn run() -> ProbeResult<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let usage = "usage: catalog_scaling_probe generate ABSENT_DIRECTORY SOURCE_COUNT | audit DIRECTORY | validate DIRECTORY | project DIRECTORY";
    let Some(mode) = args.first().and_then(|arg| arg.to_str()) else {
        return Err(invalid(usage));
    };
    let root = args
        .get(1)
        .map(PathBuf::from)
        .ok_or_else(|| invalid(usage))?;
    if !root.is_absolute() {
        return Err(invalid(
            "fixture directory must be an explicit absolute path",
        ));
    }
    let result = match mode {
        "generate" if args.len() == 3 => {
            let sources = args[2]
                .to_str()
                .ok_or_else(|| invalid("non-UTF8 count"))?
                .parse()?;
            generate(&root, sources)?
        }
        "audit" if args.len() == 2 => audit(&root)?,
        "validate" | "project" if args.len() == 2 => inspect(&root, mode)?,
        _ => return Err(invalid(usage)),
    };
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("catalog_scaling_probe: {error}");
        std::process::exit(1);
    }
}

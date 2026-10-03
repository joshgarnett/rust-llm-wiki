//! Bounded final proof and a single maintenance/reassembly retry.
use super::{context, context_types::*};
use crate::{
    catalog::{
        Catalog, CatalogGraphValidator, CatalogProjection, ReaderSnapshot, SnapshotVerification,
    },
    changes::{ChangeEngine, ProposedTarget, ScanDocument, ValidationInput},
    domain::*,
    records::{ParsedNote, parse_note},
    sources::{CitationScope, SourceView},
    vault::{ExpectedState, WriterPermit},
};
use std::{collections::BTreeMap, fs::File, io::Read, time::Instant};

pub fn context(
    catalog: &Catalog,
    writer: Option<&WriterPermit>,
    query: &str,
    request: &ContextRequest,
) -> Result<ContextResult> {
    context_with_options(catalog, writer, query, request, &ContextOptions::default())
}
struct Meter {
    budget: VerificationBudget,
    start: Instant,
    bytes: usize,
    files: usize,
    entries: usize,
}
impl Meter {
    fn new(budget: &VerificationBudget) -> Self {
        Self {
            budget: budget.clone(),
            start: Instant::now(),
            bytes: 0,
            files: 0,
            entries: 0,
        }
    }
    fn check(&self) -> Result<()> {
        if self.start.elapsed().as_millis() >= u128::from(self.budget.max_elapsed_ms) {
            return Err(budget_error("elapsed final-proof deadline exceeded"));
        }
        Ok(())
    }
    fn entry(&mut self) -> Result<()> {
        self.check()?;
        if self.entries >= self.budget.max_entries {
            return Err(budget_error(
                "final-proof directory/component/entry budget exceeded",
            ));
        }
        self.entries += 1;
        Ok(())
    }
    fn read(&mut self, catalog: &Catalog, path: &VaultRelativePath) -> Result<Option<Vec<u8>>> {
        self.check()?;
        let full = catalog
            .fs()
            .root()
            .resolve_budgeted(path, &mut || self.entry())?;
        if self.files >= self.budget.max_files {
            return Err(budget_error("final-proof file-read budget exceeded"));
        }
        self.files += 1;
        self.entry()?;
        let mut file = match File::open(&full) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io_error(e)),
        };
        self.entry()?;
        let metadata = file.metadata().map_err(io_error)?;
        if !metadata.is_file() {
            return Err(WikiError::invalid("proof target is not a regular file"));
        }
        let len = usize::try_from(metadata.len())
            .map_err(|_| budget_error("proof file length exceeds address range"))?;
        if len > self.budget.max_bytes.saturating_sub(self.bytes) {
            return Err(budget_error("final-proof byte budget exceeded before read"));
        }
        let mut bytes = vec![0u8; len];
        let mut offset = 0;
        while offset < len {
            self.check()?;
            let end = (offset + 64 * 1024).min(len);
            let read = file.read(&mut bytes[offset..end]).map_err(io_error)?;
            if read == 0 {
                return Err(conflict("proof file shrank during read"));
            }
            offset += read;
            self.bytes += read;
            self.check()?;
        }
        self.entry()?;
        if file.metadata().map_err(io_error)?.len() != metadata.len() {
            return Err(conflict("proof file length changed during read"));
        }
        self.check()?;
        Ok(Some(bytes))
    }
}
fn io_error(e: std::io::Error) -> WikiError {
    WikiError::new(ErrorCode::Internal, format!("context proof read: {e}"))
}
fn budget_error(message: &str) -> WikiError {
    let mut error = WikiError::new(ErrorCode::BudgetExceeded, message);
    error.hint = match message {
        "final-proof directory/component/entry budget exceeded" => Some(
            "Inspect vault size and layout, then rerun context with a larger --verification-max-entries allowance. Directory and path-component checks can revisit entries; this is not a unique-file count.",
        ),
        "final-proof file-read budget exceeded" => Some(
            "Inspect the evidence file count, then rerun context with a larger --verification-max-files allowance.",
        ),
        "final-proof byte budget exceeded before read" => Some(
            "Inspect evidence file sizes, then rerun context with a larger --verification-max-bytes allowance.",
        ),
        "elapsed final-proof deadline exceeded" => Some(
            "Inspect vault size and local filesystem responsiveness, then rerun context with a larger --verification-max-elapsed-ms allowance.",
        ),
        _ => None,
    }
    .map(str::to_owned);
    error
}
fn conflict(message: &str) -> WikiError {
    WikiError::new(ErrorCode::FreshnessConflict, message)
}
struct Captured {
    input: ValidationInput,
    states: BTreeMap<VaultRelativePath, ExpectedState>,
    manifest: Blake3Hash,
}
fn capture(
    catalog: &Catalog,
    projection: Option<&CatalogProjection>,
    meter: &mut Meter,
) -> Result<Captured> {
    let paths = catalog
        .fs()
        .root()
        .scan_markdown_budgeted(&mut || meter.entry())?;
    let mut notes: BTreeMap<VaultRelativePath, ParsedNote> = BTreeMap::new();
    let mut documents = Vec::new();
    let mut states = BTreeMap::new();
    let mut assets = BTreeMap::new();
    for path in paths {
        if !crate::sources::revision::canonical_path(&path) {
            continue;
        }
        let bytes = meter
            .read(catalog, &path)?
            .ok_or_else(|| conflict("canonical member disappeared during proof"))?;
        let hash = Blake3Hash::digest(&bytes);
        let note = parse_note(&bytes);
        if let Some(record) = note
            .canonical
            .as_ref()
            .filter(|r| r.kind() == RecordKind::Revision)
            && let Some((parent, _)) = path.as_str().rsplit_once('/')
        {
            for field in ["wiki_original_path", "wiki_content_path"] {
                if let Some(name) = record.string(field) {
                    assets.insert(VaultRelativePath::new(format!("{parent}/{name}"))?, ());
                }
            }
        }
        states.insert(path.clone(), ExpectedState::Hash(hash.clone()));
        notes.insert(path.clone(), note);
        documents.push(ScanDocument { path, bytes, hash });
    }
    if let Some(p) = projection {
        for d in &p.dependencies {
            if !states.contains_key(&d.path) {
                assets.insert(d.path.clone(), ());
            }
        }
    }
    let mut overlay = Vec::new();
    for path in assets.into_keys() {
        if states.contains_key(&path) {
            continue;
        }
        let bytes = meter.read(catalog, &path)?;
        let expected = bytes.as_ref().map_or(ExpectedState::Absent, |b| {
            ExpectedState::Hash(Blake3Hash::digest(b))
        });
        states.insert(path.clone(), expected);
        overlay.push(ProposedTarget { path, bytes });
    }
    meter.check()?;
    Ok(Captured {
        input: ValidationInput {
            vault_id: catalog.vault_id().clone(),
            documents,
            overlay,
        },
        states,
        manifest: crate::catalog::scan::manifest_hash(&notes),
    })
}
fn matches(
    catalog: &Catalog,
    captured: &Captured,
    reader: &ReaderSnapshot,
    meter: &Meter,
) -> Result<bool> {
    meter.check()?;
    if reader.projection().parser_fingerprint != crate::catalog::scan::parser_fingerprint()
        || captured.manifest != reader.projection().control_manifest
        || !reader
            .projection()
            .dependencies
            .iter()
            .all(|d| captured.states.get(&d.path) == Some(&d.expected))
    {
        return Ok(false);
    }
    let actual = crate::catalog::scan::project_closed(catalog.fs(), &captured.input)?;
    meter.check()?;
    Ok(actual == *reader.projection())
}
fn maintain(catalog: &Catalog, writer: &WriterPermit, meter: &Meter) -> Result<()> {
    meter.check()?;
    let engine = ChangeEngine::new(catalog.fs().clone())?;
    engine.recover(writer, &CatalogGraphValidator, catalog)?;
    meter.check()?;
    catalog.sync(writer)?;
    meter.check()
}
fn candidates(
    reader: &ReaderSnapshot,
    query: &str,
    request: &ContextRequest,
) -> Result<(super::HitSet, Option<crate::graph::GraphResult>)> {
    let hits = super::lexical::search_context(
        reader,
        query,
        &request.documents,
        request.scope != ContextScope::Current,
    )?;
    let graph = if request.target == ContextTarget::Documents {
        None
    } else {
        Some(crate::graph::query::query(
            reader,
            query,
            request.graph.as_ref().expect("validated graph plan"),
        )?)
    };
    Ok((hits, graph))
}
fn seal(
    mut draft: context::ContextDraft,
    verification: SnapshotVerification,
    meter: &Meter,
) -> ContextResult {
    draft.usage.rendered_bytes = draft.text.len();
    draft.usage.estimated_tokens = draft.text.len().div_ceil(4);
    draft.usage.verification_bytes = meter.bytes;
    draft.usage.verification_files = meter.files;
    draft.usage.verification_entries = meter.entries;
    draft.warnings.push("proof bytes/files meter canonical notes and source/dependency payloads only; directory/component/entry counts are logical operations; operational journals/SQLite/recovery/index maintenance are separate local safety work; elapsed checks do not interrupt blocking syscalls".into());
    ContextResult {
        network_used: false,
        text: draft.text,
        passages: draft.passages,
        bundles: draft.bundles,
        omissions: draft.omissions,
        usage: draft.usage,
        snapshot: draft.snapshot,
        verification,
        dependency_fingerprint: draft.dependency_fingerprint,
        truncated: draft.truncated,
        warnings: draft.warnings,
        selection_packet: draft.selection_packet,
    }
}
pub fn context_with_options(
    catalog: &Catalog,
    writer: Option<&WriterPermit>,
    query: &str,
    request: &ContextRequest,
    options: &ContextOptions,
) -> Result<ContextResult> {
    context_with_retrieval(
        catalog,
        writer,
        query,
        request,
        options,
        |reader, normalized| candidates(reader, query, normalized),
    )
}
/// Supply bounded local candidates for each pinned snapshot. Paid query preparation
/// occurs before this proof; retries retain the same total verification meter.
/// Assembly authenticates candidate locators/closure and final proof verifies bytes.
pub fn context_with_retrieval<F>(
    catalog: &Catalog,
    writer: Option<&WriterPermit>,
    query: &str,
    request: &ContextRequest,
    options: &ContextOptions,
    mut retrieve: F,
) -> Result<ContextResult>
where
    F: FnMut(
        &ReaderSnapshot,
        &ContextRequest,
    ) -> Result<(super::HitSet, Option<crate::graph::GraphResult>)>,
{
    context_with_scored_retrieval(
        catalog,
        writer,
        query,
        request,
        options,
        |reader, normalized| {
            let (hits, graph) = retrieve(reader, normalized)?;
            Ok((
                hits,
                graph,
                super::context_types::ContextSelectionSignals::default(),
            ))
        },
    )
}

pub(crate) fn context_with_scored_retrieval<F>(
    catalog: &Catalog,
    writer: Option<&WriterPermit>,
    query: &str,
    request: &ContextRequest,
    options: &ContextOptions,
    mut retrieve: F,
) -> Result<ContextResult>
where
    F: FnMut(
        &ReaderSnapshot,
        &ContextRequest,
    ) -> Result<(
        super::HitSet,
        Option<crate::graph::GraphResult>,
        super::context_types::ContextSelectionSignals,
    )>,
{
    // The caller's held-writer acquisition is outside this function. The deadline
    // starts before validation, reader opens, operational guards and maintenance.
    let mut meter = Meter::new(&request.verification_budget);
    let request = context::validate_request(query, request)?;
    context::validate_selection_action(&request, &options.selection)?;
    meter.check()?;
    if request.scope == ContextScope::Snapshot {
        let reader = catalog.index_snapshot()?;
        meter.check()?;
        let (hits, graph, signals) = retrieve(&reader, &request)?;
        let draft = context::assemble_for_query(
            &reader,
            &request,
            &hits,
            graph.as_ref(),
            query,
            &signals,
            &options.selection,
        )?;
        meter.check()?;
        return Ok(seal(draft, SnapshotVerification::IndexSnapshot, &meter));
    }
    if let Some(writer) = writer {
        writer.require_root(catalog.fs().root())?
    }
    for attempt in 0..2 {
        let proof = (|| -> Result<context::ContextDraft> {
            // Bounded preflight precedes any optional recovery/sync. It prevents an
            // obviously exhausted proof budget from causing pointless maintenance.
            let mut captured = capture(catalog, None, &mut meter)?;
            if let Err(e) = catalog.guard_current(None) {
                if e.code != ErrorCode::RecoveryRequired {
                    return Err(e);
                }
                let w = writer.ok_or(e)?;
                maintain(catalog, w, &meter)?;
                captured = capture(catalog, None, &mut meter)?;
            }
            meter.check()?;
            let reader = match catalog.index_snapshot() {
                Ok(r) => r,
                Err(e)
                    if e.code == ErrorCode::OfflineUnavailable
                        && attempt == 0
                        && writer.is_some() =>
                {
                    maintain(catalog, writer.expect("writer"), &meter)?;
                    catalog.index_snapshot()?
                }
                Err(e) => return Err(e),
            };
            meter.check()?;
            // Fill non-source projected dependencies, including explicit absences,
            // without reopening already captured canonical/source files.
            for d in &reader.projection().dependencies {
                if !captured.states.contains_key(&d.path) {
                    let bytes = meter.read(catalog, &d.path)?;
                    captured.states.insert(
                        d.path.clone(),
                        bytes.as_ref().map_or(ExpectedState::Absent, |b| {
                            ExpectedState::Hash(Blake3Hash::digest(b))
                        }),
                    );
                    captured.input.overlay.push(ProposedTarget {
                        path: d.path.clone(),
                        bytes,
                    });
                }
            }
            if !matches(catalog, &captured, &reader, &meter)? {
                return Err(conflict(
                    "initial control membership or dependency bytes differ from pinned index",
                ));
            }
            let (hits, graph, signals) = retrieve(&reader, &request)?;
            let assembled = context::assemble_for_query(
                &reader,
                &request,
                &hits,
                graph.as_ref(),
                query,
                &signals,
                &options.selection,
            )?;
            meter.check()?;
            let view = SourceView::from_closed_input(catalog.fs(), &captured.input)?;
            let scope = if request.scope == ContextScope::Current {
                CitationScope::Current
            } else {
                CitationScope::Historical
            };
            let mut selected_dependencies = BTreeMap::new();
            for passage in assembled.verification_passages() {
                for citation in &passage.citations {
                    meter.check()?;
                    let verified = view.verify(citation, scope)?;
                    for dep in verified.dependencies {
                        if captured.states.get(&dep.path) != Some(&dep.expected) {
                            return Err(conflict(
                                "selected source proof escaped captured dependency closure",
                            ));
                        }
                        selected_dependencies.insert(dep.path, dep.expected);
                    }
                }
            }
            meter.check()?;
            if let Some(fault) = &options.fault {
                fault.check(ContextCheckpoint::BeforeFinalVerification { attempt })?
            }
            meter.check()?;
            catalog.guard_current(None)?;
            meter.check()?;
            let final_input = capture(catalog, Some(reader.projection()), &mut meter)?;
            if !matches(catalog, &final_input, &reader, &meter)? {
                return Err(conflict(
                    "membership/control/dependency bytes changed immediately before context emission",
                ));
            }
            if !selected_dependencies
                .iter()
                .all(|(path, expected)| final_input.states.get(path) == Some(expected))
            {
                return Err(conflict(
                    "selected source dependency changed at final proof",
                ));
            }
            meter.check()?;
            Ok(assembled)
        })();
        match proof {
            Ok(draft) => {
                let verified_at = time::OffsetDateTime::now_utc()
                    .format(&time::format_description::well_known::Rfc3339)
                    .map_err(|e| WikiError::invalid(e.to_string()))?;
                meter.check()?;
                return Ok(seal(
                    draft,
                    SnapshotVerification::VerifiedSnapshot { verified_at },
                    &meter,
                ));
            }
            Err(e)
                if matches!(
                    e.code,
                    ErrorCode::FreshnessConflict | ErrorCode::RecoveryRequired
                ) && attempt == 0
                    && writer.is_some() =>
            {
                maintain(catalog, writer.expect("writer"), &meter)?;
            }
            Err(e) => return Err(e),
        }
    }
    Err(conflict("context failed its one refresh retry"))
}

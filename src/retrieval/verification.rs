//! Bounded final proof and a single maintenance/reassembly retry.
use super::{context, context_types::*};
use crate::{
    catalog::{
        Catalog, CatalogGraphValidator, CatalogProjection, ReaderSnapshot, SnapshotVerification,
        query::QuerySnapshot, query_types::QueryReadLimits,
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

/// Selected canonical reads consume the same allowance as the complete current
/// observation. Neither admission nor its final recheck starts a new budget.
pub(crate) fn current_proof_budget(
    reader: &QuerySnapshot,
    requested: &VerificationBudget,
) -> Result<VerificationBudget> {
    let observation = reader.current_observation().ok_or_else(|| {
        WikiError::invalid("current document proof requires a current observation")
    })?;
    let (bytes, entries, elapsed_ms) = observation.remaining_work()?;
    let budget = VerificationBudget {
        max_bytes: requested.max_bytes.min(bytes),
        // Each selected physical read and logical entry shares the observation's
        // path-work allowance. Reserving half for each prevents over-admission.
        max_files: requested.max_files.min(entries / 2),
        max_entries: requested.max_entries.min(entries / 2),
        max_elapsed_ms: requested.max_elapsed_ms.min(elapsed_ms),
    };
    if budget.max_bytes == 0
        || budget.max_files == 0
        || budget.max_entries == 0
        || budget.max_elapsed_ms == 0
    {
        return Err(budget_error(
            "current observation proof allowance exhausted",
        ));
    }
    Ok(budget)
}

/// This is the only seal for current normalized evidence. Selected citation
/// proof is complete before the full membership, bytes and authority recheck.
pub(crate) fn verify_current_observation(
    catalog: &Catalog,
    reader: &QuerySnapshot,
    proof: &super::selected_documents::SelectedDocuments,
) -> Result<SnapshotVerification> {
    let observation = reader.current_observation().ok_or_else(|| {
        WikiError::invalid("current document proof requires a current observation")
    })?;
    let (bytes, files, entries) = proof.meter().work();
    observation.charge_work(bytes, files, entries)?;
    reader.final_recheck(catalog)?;
    Ok(SnapshotVerification::VerifiedSnapshot {
        verified_at: crate::sources::revision::timestamp()?,
    })
}
pub(crate) struct Meter {
    budget: VerificationBudget,
    start: Instant,
    bytes: usize,
    files: usize,
    entries: usize,
    file_byte_limits: BTreeMap<VaultRelativePath, usize>,
    optional_display: bool,
    last_read_path: Option<std::path::PathBuf>,
}
impl Meter {
    pub(crate) fn new(budget: &VerificationBudget) -> Self {
        Self {
            budget: budget.clone(),
            start: Instant::now(),
            bytes: 0,
            files: 0,
            entries: 0,
            file_byte_limits: BTreeMap::new(),
            optional_display: false,
            last_read_path: None,
        }
    }
    pub(super) fn check(&self) -> Result<()> {
        if self.start.elapsed().as_millis() >= u128::from(self.budget.max_elapsed_ms) {
            return Err(budget_error("elapsed final-proof deadline exceeded"));
        }
        Ok(())
    }
    pub(super) fn remaining_ms(&self) -> u64 {
        self.budget
            .max_elapsed_ms
            .saturating_sub(u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX))
    }
    pub(crate) fn work(&self) -> (usize, usize, usize) {
        (self.bytes, self.files, self.entries)
    }
    /// An earlier owner proof cannot retain a larger allowance than the
    /// operation has left when its final recheck starts. Never reset usage.
    pub(super) fn tighten_remaining(&mut self, remaining: &VerificationBudget) -> Result<()> {
        self.check()?;
        if remaining.max_elapsed_ms == 0 {
            return Err(budget_error("elapsed final-proof deadline exceeded"));
        }
        self.budget.max_bytes = self
            .budget
            .max_bytes
            .min(self.bytes.saturating_add(remaining.max_bytes));
        self.budget.max_files = self
            .budget
            .max_files
            .min(self.files.saturating_add(remaining.max_files));
        self.budget.max_entries = self
            .budget
            .max_entries
            .min(self.entries.saturating_add(remaining.max_entries));
        self.budget.max_elapsed_ms = self.budget.max_elapsed_ms.min(
            u64::try_from(self.start.elapsed().as_millis())
                .unwrap_or(u64::MAX)
                .saturating_add(remaining.max_elapsed_ms),
        );
        self.check()
    }
    /// Optional display reads share accounting but cannot spend the resources
    /// held for the mandatory final evidence pass. Failed work is charged too.
    pub(crate) fn read_optional(
        &mut self,
        catalog: &Catalog,
        path: &VaultRelativePath,
        byte_limit: usize,
        reserve: (usize, usize, usize),
        remaining_ms: u64,
    ) -> Option<(Vec<u8>, std::path::PathBuf)> {
        let allowance = VerificationBudget {
            max_bytes: self
                .budget
                .max_bytes
                .saturating_sub(self.bytes)
                .saturating_sub(reserve.0)
                .min(byte_limit),
            max_files: self
                .budget
                .max_files
                .saturating_sub(self.files)
                .saturating_sub(reserve.1),
            max_entries: self
                .budget
                .max_entries
                .saturating_sub(self.entries)
                .saturating_sub(reserve.2),
            max_elapsed_ms: self.remaining_ms().min(remaining_ms),
        };
        if allowance.max_bytes == 0
            || allowance.max_files == 0
            || allowance.max_entries == 0
            || allowance.max_elapsed_ms == 0
        {
            return None;
        }
        let mut optional = Meter::new(&allowance);
        optional.optional_display = true;
        optional.file_byte_limits.insert(path.clone(), byte_limit);
        let result = (|| -> Result<_> {
            let bytes = optional.read(catalog, path)?;
            Ok(bytes.and_then(|bytes| optional.last_read_path.take().map(|full| (bytes, full))))
        })();
        self.bytes += optional.bytes;
        self.files += optional.files;
        self.entries += optional.entries;
        result.ok().flatten()
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
    /// Additional request-local input bounds survive the selected proof recheck.
    pub(super) fn limit_file_bytes(&mut self, limits: &BTreeMap<VaultRelativePath, usize>) {
        self.file_byte_limits = limits.clone();
    }
    /// Tighten additional optional-artifact caps without relaxing existing limits.
    pub(super) fn extend_file_byte_limits(&mut self, limits: &BTreeMap<VaultRelativePath, usize>) {
        for (path, limit) in limits {
            self.file_byte_limits
                .entry(path.clone())
                .and_modify(|old| *old = (*old).min(*limit))
                .or_insert(*limit);
        }
    }
    pub(super) fn preflight_file_length(
        &mut self,
        catalog: &Catalog,
        path: &VaultRelativePath,
    ) -> Result<usize> {
        self.check()?;
        let full = catalog
            .fs()
            .root()
            .resolve_budgeted(path, &mut || self.entry())?;
        if self.files >= self.budget.max_files {
            return Err(budget_error("original-input file budget exceeded"));
        }
        self.files += 1;
        self.entry()?;
        let metadata = std::fs::metadata(full).map_err(io_error)?;
        self.check()?;
        if !metadata.is_file() {
            return Err(WikiError::invalid("original input is not a regular file"));
        }
        usize::try_from(metadata.len())
            .map_err(|_| budget_error("original-input file length exceeds address range"))
    }
    pub(super) fn read(
        &mut self,
        catalog: &Catalog,
        path: &VaultRelativePath,
    ) -> Result<Option<Vec<u8>>> {
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
        self.last_read_path = Some(full.clone());
        let opened = if self.optional_display {
            // A FIFO or symlink swapped after resolution must not turn optional
            // navigation into an unbounded blocking read.
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                    .open(&full)
            }
            #[cfg(not(unix))]
            {
                File::open(&full)
            }
        } else {
            File::open(&full)
        };
        let mut file = match opened {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io_error(e)),
        };
        #[cfg(test)]
        crate::catalog::query_diagnostics::read("proof-open", &full, 0);
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
        if self
            .file_byte_limits
            .get(path)
            .is_some_and(|limit| len > *limit)
        {
            return Err(budget_error(
                "original-input file grew beyond its admitted byte bound",
            ));
        }
        let mut bytes = vec![0u8; len];
        let mut offset = 0;
        while offset < len {
            self.check()?;
            let end = (offset + 64 * 1024).min(len);
            let read = file.read(&mut bytes[offset..end]).map_err(io_error)?;
            #[cfg(test)]
            crate::catalog::query_diagnostics::read("proof-read", &full, read);
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
            "Increase --verification-max-entries only if below its ceiling. At the ceiling, this vault exceeds the supported entry budget for verified context. Directory and path-component checks can revisit entries; this is not a unique-file count.",
        ),
        "final-proof file-read budget exceeded" => Some(
            "Increase --verification-max-files only if below its ceiling. At the ceiling, this vault exceeds the supported file-read budget for verified context.",
        ),
        "final-proof byte budget exceeded before read" => Some(
            "Increase --verification-max-bytes only if below its ceiling. At the ceiling, this vault exceeds the supported byte budget for verified context.",
        ),
        "elapsed final-proof deadline exceeded" => Some(
            "Inspect local filesystem responsiveness; increase --verification-max-elapsed-ms only if below its ceiling. At the ceiling, verification cannot finish within the supported deadline.",
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
// Only already admitted document and assertion evidence owners are considered.
fn link_documents<'a>(
    reader: &'a ReaderSnapshot,
    hits: &super::HitSet,
    graph: Option<&crate::graph::GraphResult>,
) -> Vec<&'a crate::catalog::DocumentRow> {
    let mut paths = std::collections::BTreeSet::new();
    let mut documents = Vec::new();
    for hit in &hits.hits {
        if paths.insert(hit.locator.path.clone())
            && let Some(document) = reader
                .projection()
                .documents
                .iter()
                .find(|document| document.path == hit.locator.path)
        {
            documents.push(document);
        }
    }
    let assertions = graph
        .into_iter()
        .flat_map(|graph| &graph.assertions)
        .map(|assertion| assertion.record_ref.record_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let revisions = reader
        .projection()
        .records
        .values()
        .filter(|row| {
            row.record.kind() == RecordKind::Evidence
                && row
                    .record
                    .string("wiki_assertion_id")
                    .is_some_and(|id| assertions.contains(id))
        })
        .filter_map(|row| row.record.string("wiki_source_revision"))
        .collect::<std::collections::BTreeSet<_>>();
    for document in &reader.projection().documents {
        if document
            .owner_revision
            .as_ref()
            .is_some_and(|id| revisions.contains(id.as_str()))
            && paths.insert(document.path.clone())
        {
            documents.push(document);
        }
    }
    documents
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
pub(super) fn seal(
    mut draft: context::ContextDraft,
    verification: SnapshotVerification,
    meter: &Meter,
) -> ContextResult {
    draft.usage.rendered_bytes = draft.text.len();
    draft.usage.estimated_tokens = draft.text.len().div_ceil(4);
    draft.usage.verification_bytes = meter.bytes;
    draft.usage.verification_files = meter.files;
    draft.usage.verification_entries = meter.entries;
    draft.warnings.push("proof bytes/files meter canonical notes, source/dependency payloads and optional saved-display reads; directory/component/entry counts are logical operations; operational journals/SQLite/recovery/index maintenance are separate local safety work; elapsed checks do not interrupt blocking syscalls".into());
    ContextResult {
        network_used: false,
        text: draft.text,
        human_text: draft.human_text,
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
    context::validate_source_relations(request, options)?;
    if options.experimental_hybrid_lexical_evidence {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "experimental hybrid lexical evidence requires the indexed semantic coordinator",
        ));
    }
    if request.scope == ContextScope::IndexedEvidence {
        return super::indexed_context::context(catalog, query, request, options);
    }
    if request.scope == ContextScope::IndexedDocuments {
        return super::indexed_documents::context(catalog, query, request, options);
    }
    if catalog.operation_state()?.is_some() {
        let meter = Meter::new(&request.verification_budget);
        let request = context::validate_request(query, request)?;
        context::validate_selection_action(&request, &options.selection)?;
        if request.scope != ContextScope::Snapshot
            || request.target != ContextTarget::Documents
            || request.documents.mode != super::SearchMode::Lexical
            || request.graph.is_some()
        {
            return Err(WikiError::new(
                ErrorCode::Usage,
                "normalized context supports lexical document snapshots and indexed-evidence only",
            ));
        }
        meter.check()?;
        catalog.guard_query()?;
        meter.check()?;
        let reader = catalog.cached_query_snapshot(QueryReadLimits::default())?;
        let hits =
            super::lexical::search_context_catalog(&reader, query, &request.documents, true)?;
        let draft = context::assemble_bounded_documents_for_query(&reader, &request, &hits, query)?;
        meter.check()?;
        if let Some(fault) = &options.fault {
            fault.check(ContextCheckpoint::BeforeFinalVerification { attempt: 0 })?;
        }
        reader.verify_operations(catalog)?;
        meter.check()?;
        return Ok(seal(draft, SnapshotVerification::IndexSnapshot, &meter));
    }
    context_with_retrieval(
        catalog,
        writer,
        query,
        request,
        options,
        |reader, normalized| candidates(reader, query, normalized),
    )
}

/// Assemble ordinary local context from one coordinator-owned legacy reader.
/// This entrypoint never repairs, synchronizes or acquires write authority.
/// A verified reader's membership and exact dependencies are rechecked without
/// rebuilding its retrieval projection; explicit snapshots remain uncited.
pub(crate) fn context_with_reader(
    catalog: &Catalog,
    reader: &ReaderSnapshot,
    read_budget: &crate::catalog::scan::ReadScanBudget,
    query: &str,
    request: &ContextRequest,
    options: &ContextOptions,
) -> Result<ContextResult> {
    context::validate_source_relations(request, options)?;
    if options.experimental_hybrid_lexical_evidence {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "experimental hybrid lexical evidence requires the indexed semantic coordinator",
        ));
    }
    let (bytes, files, entries) = read_budget.remaining();
    let mut allowance = request.verification_budget.clone();
    allowance.max_bytes = allowance.max_bytes.min(bytes);
    allowance.max_files = allowance.max_files.min(files);
    allowance.max_entries = allowance.max_entries.min(entries);
    allowance.max_elapsed_ms = allowance.max_elapsed_ms.min(
        u64::try_from(
            read_budget
                .deadline()
                .saturating_duration_since(Instant::now())
                .as_millis(),
        )
        .unwrap_or(u64::MAX),
    );
    let mut meter = Meter::new(&allowance);
    let request = context::validate_request(query, request)?;
    context::validate_selection_action(&request, &options.selection)?;
    if matches!(
        request.scope,
        ContextScope::IndexedEvidence | ContextScope::IndexedDocuments
    ) {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "indexed evidence scopes require their bounded lexical coordinator",
        ));
    }
    meter.check()?;
    if catalog.operation_state()?.is_some() {
        return Err(WikiError::new(
            ErrorCode::RecoveryRequired,
            "normalized authority activated while a legacy context reader was held",
        ));
    }
    if request.scope == ContextScope::Snapshot {
        let (hits, graph) = candidates(reader, query, &request)?;
        let draft = context::assemble_for_query(
            reader,
            &request,
            &hits,
            graph.as_ref(),
            query,
            &ContextSelectionSignals::default(),
            &options.selection,
        )?;
        meter.check()?;
        if let Some(fault) = &options.fault {
            fault.check(ContextCheckpoint::BeforeFinalVerification { attempt: 0 })?;
        }
        if catalog.operation_state()?.is_some() {
            return Err(WikiError::new(
                ErrorCode::RecoveryRequired,
                "normalized authority activated while a legacy context reader was held",
            ));
        }
        let (bytes, files, entries) = meter.work();
        read_budget.charge_observation(bytes, files, entries)?;
        return Ok(seal(draft, SnapshotVerification::IndexSnapshot, &meter));
    }
    if !matches!(
        reader.verification(),
        SnapshotVerification::VerifiedSnapshot { .. }
    ) {
        return Err(conflict(
            "current context requires a verified read-only snapshot",
        ));
    }
    catalog.guard_current(None)?;
    let captured = capture(catalog, Some(reader.projection()), &mut meter)?;
    let matches_reader = |captured: &Captured| {
        captured.manifest == reader.projection().control_manifest
            && reader.projection().parser_fingerprint == crate::catalog::scan::parser_fingerprint()
            && reader.projection().dependencies.iter().all(|dependency| {
                captured.states.get(&dependency.path) == Some(&dependency.expected)
            })
    };
    if !matches_reader(&captured) {
        return Err(conflict(
            "canonical membership or dependencies changed after read-only snapshot verification",
        ));
    }
    let (hits, graph) = candidates(reader, query, &request)?;
    let documents = link_documents(reader, &hits, graph.as_ref());
    let reserve = meter.work();
    let mut links = if matches!(
        options.selection,
        super::context_selection_packet::SelectionAction::Prepare
    ) {
        super::source_links::SourceLinkProof::empty()
    } else {
        super::source_links::SourceLinkProof::observe(
            catalog,
            &reader.projection().records,
            documents,
            &mut meter,
            reserve,
        )
    };
    let signals = ContextSelectionSignals {
        source_links: links.links.clone(),
        ..Default::default()
    };
    let mut draft = context::assemble_for_query(
        reader,
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
    for passage in draft.verification_passages() {
        for citation in &passage.citations {
            meter.check()?;
            for dependency in view.verify(citation, scope)?.dependencies {
                if captured.states.get(&dependency.path) != Some(&dependency.expected) {
                    return Err(conflict(
                        "selected source proof escaped captured dependency closure",
                    ));
                }
                selected_dependencies.insert(dependency.path, dependency.expected);
            }
        }
    }
    meter.check()?;
    if let Some(fault) = &options.fault {
        fault.check(ContextCheckpoint::BeforeFinalVerification { attempt: 0 })?;
    }
    if links.recheck(catalog, &mut meter) {
        draft.retain_source_links(reader, &request, &links.links)?;
    }
    catalog.guard_current(None)?;
    let final_input = capture(catalog, Some(reader.projection()), &mut meter)?;
    if !matches_reader(&final_input)
        || !selected_dependencies
            .iter()
            .all(|(path, expected)| final_input.states.get(path) == Some(expected))
    {
        return Err(conflict(
            "canonical membership or selected evidence changed before context emission",
        ));
    }
    catalog.guard_current(None)?;
    meter.check()?;
    let verified_at = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|error| WikiError::invalid(error.to_string()))?;
    let (bytes, files, entries) = meter.work();
    read_budget.charge_observation(bytes, files, entries)?;
    Ok(seal(
        draft,
        SnapshotVerification::VerifiedSnapshot { verified_at },
        &meter,
    ))
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
    context::validate_source_relations(request, options)?;
    if options.experimental_hybrid_lexical_evidence {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "experimental hybrid lexical evidence requires the indexed semantic coordinator",
        ));
    }
    let mut meter = Meter::new(&request.verification_budget);
    let request = context::validate_request(query, request)?;
    context::validate_selection_action(&request, &options.selection)?;
    meter.check()?;
    if matches!(
        request.scope,
        ContextScope::IndexedEvidence | ContextScope::IndexedDocuments
    ) {
        return Err(WikiError::new(
            ErrorCode::Usage,
            "indexed evidence scopes require their bounded lexical coordinator",
        ));
    }
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
        if let Some(fault) = &options.fault {
            fault.check(ContextCheckpoint::BeforeFinalVerification { attempt: 0 })?;
        }
        if catalog.operation_state()?.is_some() {
            return Err(WikiError::new(
                ErrorCode::RecoveryRequired,
                "normalized authority activated while a legacy context reader was held",
            ));
        }
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
            let (hits, graph, mut signals) = retrieve(&reader, &request)?;
            let documents = link_documents(&reader, &hits, graph.as_ref());
            let reserve = meter.work();
            let mut links = if matches!(
                options.selection,
                super::context_selection_packet::SelectionAction::Prepare
            ) {
                super::source_links::SourceLinkProof::empty()
            } else {
                super::source_links::SourceLinkProof::observe(
                    catalog,
                    &reader.projection().records,
                    documents,
                    &mut meter,
                    reserve,
                )
            };
            signals.source_links = links.links.clone();
            let mut assembled = context::assemble_for_query(
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
            if links.recheck(catalog, &mut meter) {
                assembled.retain_source_links(&reader, &request, &links.links)?;
            }
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

#[cfg(test)]
mod read_only_context_tests {
    use super::*;
    use crate::vault::{VaultFs, VaultRoot};
    use std::{path::PathBuf, sync::Arc};

    fn fixture() -> (tempfile::TempDir, Catalog) {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("WIKI.md"), b"---\nwiki_schema: '1'\nwiki_id: vault_context_read_only\nwiki_kind: vault\ntitle: Read-only context\n---\n").unwrap();
        std::fs::write(temp.path().join("page.md"), b"---\nwiki_schema: '1'\nwiki_id: page_context_read_only\nwiki_kind: page\ntitle: Cabinet\nwiki_status: reviewed\n---\nCobalt cabinet requires both keys.\n").unwrap();
        let catalog = Catalog::new(
            VaultFs::new(VaultRoot::explicit(temp.path()).unwrap()),
            RecordId::new("vault_context_read_only").unwrap(),
        );
        (temp, catalog)
    }

    struct ChangeBeforeEmission(PathBuf);
    impl ContextFault for ChangeBeforeEmission {
        fn check(&self, _: ContextCheckpoint) -> Result<()> {
            let current = std::fs::read_to_string(&self.0).unwrap();
            std::fs::write(&self.0, current.replace("both keys", "one key")).unwrap();
            Ok(())
        }
    }

    #[test]
    fn held_read_only_context_rejects_external_change_at_final_proof() {
        let (temp, catalog) = fixture();
        let budget = crate::catalog::scan::ReadScanBudget::new(Default::default()).unwrap();
        let reader = catalog.read_only_snapshot_with_budget(&budget).unwrap();
        let error = context_with_reader(
            &catalog,
            &reader,
            &budget,
            "Cobalt",
            &ContextRequest::default(),
            &ContextOptions {
                fault: Some(Arc::new(ChangeBeforeEmission(temp.path().join("page.md")))),
                ..Default::default()
            },
        )
        .err()
        .unwrap();
        assert_eq!(error.code, ErrorCode::FreshnessConflict);
        assert!(
            std::fs::read_to_string(temp.path().join("page.md"))
                .unwrap()
                .contains("one key")
        );
        assert!(!temp.path().join(".wiki").exists());
    }
}

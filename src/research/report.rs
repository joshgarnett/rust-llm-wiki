//! Current provenance, unassessed page proposals, and immutable local reports.
use super::{
    ResearchGap, ResearchPassage, ResearchReport, ResearchScope, frontier, plan, stages,
    synthesis::{self, ClaimStatus, ResearchProposal, ValidatedSynthesis},
};
use crate::{
    catalog::{Catalog, CatalogGraphValidator},
    changes::{prepare::read_bounded, *},
    domain::*,
    graph::packet::canonical_json,
    jobs::*,
    providers::types::{RemoteInput, RemoteOperation},
    records::parse_note,
    sources::{CitationScope, SourceView},
    vault::{DirectorySync, ExpectedState, VaultFs, WriterPermit},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

const MAX_BYTES: usize = 256 * 1024;
const MARKER: &str = "research_proposal";
const WARNING: &str = "Unassessed model-generated proposal. Citations establish provenance only; claims require review.";
fn invalid(message: &str) -> WikiError {
    WikiError::invalid(message)
}
fn gap(message: String, code: &str) -> ResearchGap {
    ResearchGap {
        stage: TaskStage::StageChanges,
        task_key: None,
        origin: None,
        code: code.into(),
        message,
    }
}
fn report_hash(report: &ResearchReport) -> Result<Blake3Hash> {
    let bytes = canonical_json(report)?;
    if bytes.len() > MAX_BYTES {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "research report exceeds 256 KiB",
        ));
    }
    Ok(Blake3Hash::digest(bytes))
}
fn append_dependencies(
    into: &mut BTreeMap<VaultRelativePath, ExpectedState>,
    dependencies: &[ReadDependency],
) -> Result<()> {
    for dependency in dependencies {
        if into
            .insert(dependency.path.clone(), dependency.expected.clone())
            .is_some_and(|old| old != dependency.expected)
        {
            return Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                "research report read proofs conflict",
            ));
        }
    }
    Ok(())
}
fn dependencies(map: BTreeMap<VaultRelativePath, ExpectedState>) -> Vec<ReadDependency> {
    map.into_iter()
        .map(|(path, expected)| ReadDependency { path, expected })
        .collect()
}
fn task_bytes(fs: &VaultFs, task: &TaskSpec, run: &RecordId) -> Result<Vec<u8>> {
    if task.key != crate::jobs::tasks::task_key(task)?
        || task.input_hash != task.input.hash
        || task.input.path
            != VaultRelativePath::new(format!("runs/{run}/inputs/{}.json", task.input.hash.hex()))?
    {
        return Err(invalid("research report descriptor identity differs"));
    }
    let bytes = read_bounded(fs, &task.input.path, MAX_BYTES)?.ok_or_else(|| {
        WikiError::new(
            ErrorCode::RecoveryRequired,
            "research report descriptor missing",
        )
    })?;
    if bytes.len() as u64 != task.input.byte_len || Blake3Hash::digest(&bytes) != task.input.hash {
        return Err(WikiError::new(
            ErrorCode::FreshnessConflict,
            "research report descriptor changed",
        ));
    }
    Ok(bytes)
}
fn owned_output(
    fs: &VaultFs,
    inspection: &LedgerInspection,
    task: &TaskSpec,
    output: &DurableOutputRef,
) -> Result<stages::StageOutput> {
    if output.record.vault_id != inspection.spec.vault_id
        || output.record.expected_kind != RecordKind::RunEvent
        || output.path
            != VaultRelativePath::new(format!(
                "runs/{}/outputs/{}.md",
                inspection.spec.run_id, output.record.record_id
            ))?
    {
        return Err(invalid("research report output vault or path differs"));
    }
    let stage = stages::read_output(fs, output, task)?;
    let bytes = read_bounded(fs, &output.path, 512 * 1024)?
        .ok_or_else(|| invalid("research output missing"))?;
    let note = parse_note(&bytes);
    if note
        .canonical
        .as_ref()
        .and_then(|r| r.string("wiki_run_id"))
        != Some(inspection.spec.run_id.as_str())
    {
        return Err(invalid("research report output run owner differs"));
    }
    Ok(stage)
}
/// Recover the immutable whitelist from the actual completed paid synthesis.
fn synthesis_binding(
    fs: &VaultFs,
    inspection: &LedgerInspection,
    scope: &ResearchScope,
    output: &synthesis::Synthesis,
) -> Result<(plan::ResearchStageBinding, Vec<ReadDependency>, Blake3Hash)> {
    let expected = canonical_json(output)?;
    let mut matches = Vec::new();
    for task in inspection.tasks.values() {
        if task.state != TaskState::Completed
            || task.spec.stage != TaskStage::Synthesize
            || task.spec.capability != Some(Capability::Generate)
            || inspection
                .research
                .as_ref()
                .is_some_and(|r| !r.active_tasks.contains(&task.spec.key))
        {
            continue;
        }
        for reference in &task.outputs {
            let stage = owned_output(fs, inspection, &task.spec, reference)?;
            if canonical_json(&stage.response)? != expected {
                continue;
            }
            let attempt = stage
                .attempt
                .as_ref()
                .ok_or_else(|| invalid("synthesis has no paid attempt"))?;
            if !inspection.attempts.iter().any(|a| {
                &a.attempt == attempt && a.receipt.is_some() && a.outputs.contains(reference)
            }) {
                return Err(invalid(
                    "synthesis output lacks acknowledged paid ownership",
                ));
            }
            let bytes = task_bytes(fs, &task.spec, &inspection.spec.run_id)?;
            let input: RemoteInput = crate::changes::prepare::strict_json(&bytes)?;
            if canonical_json(&input)? != bytes {
                return Err(invalid("synthesis descriptor is not canonical"));
            }
            let RemoteOperation::Generate { data, .. } = input.operation else {
                return Err(invalid("synthesis descriptor is not generation"));
            };
            let value: serde_json::Value = crate::changes::prepare::strict_json(data.as_bytes())?;
            let binding: plan::ResearchStageBinding = serde_json::from_value(
                value
                    .get("binding")
                    .cloned()
                    .ok_or_else(|| invalid("synthesis binding missing"))?,
            )
            .map_err(|_| invalid("synthesis binding invalid"))?;
            if binding.version != 1
                || binding.stage != TaskStage::Synthesize
                || binding.scope_hash != Blake3Hash::digest(canonical_json(scope)?)
                || binding.round == 0
                || binding.round > scope.limits.rounds
                || binding.current_records.len() > crate::changes::prepare::MAX_OPS
                || binding.citations.len() > scope.limits.stage.max_citations
            {
                return Err(invalid("synthesis scope or whitelist differs"));
            }
            matches.push((
                binding,
                task.spec.source_bindings.clone(),
                task.spec.key.clone(),
            ));
        }
    }
    matches
        .pop()
        .ok_or_else(|| invalid("synthesis is not an active completed paid stage"))
}
fn citation_dependencies(
    view: &SourceView<'_>,
    refs: &[CitationRef],
) -> Result<Vec<ReadDependency>> {
    let mut reads = BTreeMap::new();
    for citation in refs {
        append_dependencies(
            &mut reads,
            &view.verify(citation, CitationScope::Current)?.dependencies,
        )?;
    }
    Ok(dependencies(reads))
}
fn proposal_citations(proposal: &ResearchProposal) -> &[CitationRef] {
    match proposal {
        ResearchProposal::CreatePage { citations, .. }
        | ResearchProposal::UpdatePage { citations, .. } => citations,
    }
}
fn proposal_marker(run: &RecordId, proposal: &ResearchProposal) -> Result<RecordId> {
    RecordId::new(format!(
        "research_proposal_{}",
        Blake3Hash::digest(canonical_json(&(
            "lwiki.research-proposal.v1",
            run,
            proposal
        ))?)
        .hex()
    ))
}
fn create_choice(
    run: &RecordId,
    proposal: &ResearchProposal,
) -> Result<(RecordId, VaultRelativePath)> {
    let hash = Blake3Hash::digest(canonical_json(&("lwiki.research-page.v1", run, proposal))?);
    Ok((
        RecordId::new(format!("page_research_{}", hash.hex()))?,
        VaultRelativePath::new(format!("pages/research_{}.md", hash.hex()))?,
    ))
}
fn page_bytes(
    proposal: &ResearchProposal,
    page: &RecordId,
    before: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let (fields, body) = match proposal {
        ResearchProposal::CreatePage { title, body, .. } => (
            BTreeMap::from([
                ("wiki_schema".into(), serde_json::json!("1")),
                ("wiki_id".into(), serde_json::json!(page)),
                ("wiki_kind".into(), serde_json::json!("page")),
                ("wiki_status".into(), serde_json::json!("draft")),
                (
                    "title".into(),
                    serde_json::json!(format!("Unassessed research: {title}")),
                ),
            ]),
            body,
        ),
        ResearchProposal::UpdatePage { record, body, .. } => {
            let note =
                parse_note(before.ok_or_else(|| invalid("update page before bytes missing"))?);
            let original = note
                .canonical
                .filter(|r| r.id() == &record.record_id && r.kind() == RecordKind::Page)
                .ok_or_else(|| invalid("research update must preserve page identity"))?;
            if &record.record_id != page {
                return Err(invalid("research page allocation differs"));
            }
            (original.into_fields(), body)
        }
    };
    CanonicalRecord::new(fields.clone())?;
    let mut bytes = b"---\n".to_vec();
    for (key, value) in fields {
        bytes.extend_from_slice(
            format!(
                "{key}: {}\n",
                serde_json::to_string(&value)
                    .map_err(|_| invalid("research page field encoding"))?
            )
            .as_bytes(),
        );
    }
    bytes.extend_from_slice(format!("---\n\n> {WARNING}\n\n{body}\n\n").as_bytes());
    bytes.extend_from_slice(&crate::graph::packet::render_fence(
        &proposal_citations(proposal),
        "lwiki-research-citations-v1",
        MAX_BYTES,
    )?);
    if bytes.len() > MAX_BYTES {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "research page proposal exceeds 256 KiB",
        ));
    }
    Ok(bytes)
}
fn current_page(
    fs: &VaultFs,
    vault: &RecordId,
    record: &RecordRef,
) -> Result<(VaultRelativePath, Vec<u8>)> {
    if &record.vault_id != vault || record.expected_kind != RecordKind::Page {
        return Err(invalid("research update vault/kind differs"));
    }
    let mut found = None;
    let paths = fs.root().scan_markdown()?;
    if paths.len() > 65_536 {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "research page scan exceeds ceiling",
        ));
    }
    for path in paths {
        let bytes = read_bounded(fs, &path, crate::changes::prepare::MAX_PAYLOAD_BYTES)?
            .ok_or_else(|| invalid("research page disappeared"))?;
        let parsed = parse_note(&bytes);
        if parsed
            .canonical
            .as_ref()
            .is_some_and(|r| r.id() == &record.record_id)
        {
            if parsed
                .canonical
                .as_ref()
                .is_none_or(|r| r.kind() != RecordKind::Page)
                || found.is_some()
            {
                return Err(invalid("research page identity is ambiguous"));
            }
            found = Some((path, bytes));
        }
    }
    found.ok_or_else(|| WikiError::new(ErrorCode::RecordNotFound, "research update page missing"))
}
fn observed_state(fs: &VaultFs, path: &VaultRelativePath) -> Result<ExpectedState> {
    Ok(
        read_bounded(fs, path, crate::changes::prepare::MAX_PAYLOAD_BYTES)?
            .map_or(ExpectedState::Absent, |bytes| {
                ExpectedState::Hash(Blake3Hash::digest(bytes))
            }),
    )
}
fn change_stale(fs: &VaultFs, actual: &ChangeInspection) -> Result<bool> {
    for read in &actual.manifest.read_preconditions {
        if observed_state(fs, &read.path)? != read.expected {
            return Ok(true);
        }
    }
    for op in &actual.manifest.operations {
        let expected = if actual.status == ChangeStatus::Committed {
            &op.after
        } else {
            &op.before
        };
        if &observed_state(fs, &op.target)? != expected {
            return Ok(true);
        }
    }
    Ok(matches!(
        actual.status,
        ChangeStatus::Conflict | ChangeStatus::Aborted
    ))
}
fn inspect_change(engine: &ChangeEngine, change: &PreparedChange) -> Result<ChangeInspection> {
    let actual = engine.inspect(&change.change_id)?;
    if &actual.prepared != change {
        return Err(invalid("research prepared manifest hash differs"));
    }
    // inspect loads the canonical manifest and verifies every retained payload.
    Ok(actual)
}
fn verify_page_change(
    fs: &VaultFs,
    engine: &ChangeEngine,
    actual: &ChangeInspection,
    run: &RecordId,
    proposal: &ResearchProposal,
    reads: &[ReadDependency],
    admitted_reads: &[ReadDependency],
) -> Result<()> {
    let manifest = &actual.manifest;
    if manifest.origin.is_some()
        || manifest.inverse_of.is_some()
        || manifest.operations.len() != 1
        || manifest.allocated_ids.get(MARKER) != Some(&proposal_marker(run, proposal)?)
        || manifest.allocated_ids.len() != 2
        || manifest.read_preconditions != reads
    {
        return Err(invalid("retained research page change proof differs"));
    }
    let page = manifest
        .allocated_ids
        .get("page")
        .ok_or_else(|| invalid("research page allocation missing"))?;
    let op = &manifest.operations[0];
    if op.role != OperationRole::MutableRecord || !op.apply_after.is_empty() {
        return Err(invalid("research page operation role differs"));
    }
    let before = engine.verify_payload(
        &manifest.change_id,
        0,
        "before",
        &op.target,
        &op.before,
        &op.before_payload,
    )?;
    match proposal {
        ResearchProposal::CreatePage { .. } => {
            let (expected_id, expected_path) = create_choice(run, proposal)?;
            if page != &expected_id
                || op.target != expected_path
                || op.before != ExpectedState::Absent
            {
                return Err(invalid("research create target differs"));
            }
        }
        ResearchProposal::UpdatePage { record, .. } => {
            if page != &record.record_id || !matches!(op.before, ExpectedState::Hash(_)) {
                return Err(invalid("research update identity differs"));
            }
            if !crate::sources::revision::canonical_path(&op.target) {
                return Err(invalid("research update target is not canonical Markdown"));
            }
            if current_page(fs, engine.vault_id(), record)?.0 != op.target {
                return Err(invalid(
                    "research update target no longer owns its allowed page",
                ));
            }
            if admitted_page_state(admitted_reads, &op.target)? != op.before {
                return Err(invalid(
                    "research update expected state differs from paid synthesis proof",
                ));
            }
        }
    }
    let expected = page_bytes(proposal, page, before.as_deref())?;
    let after = engine.verify_payload(
        &manifest.change_id,
        0,
        "proposed",
        &op.target,
        &op.after,
        &op.after_payload,
    )?;
    if after.as_deref() != Some(expected.as_slice())
        || op.after != ExpectedState::Hash(Blake3Hash::digest(&expected))
    {
        return Err(invalid("retained research page bytes differ"));
    }
    // Never infer authority from the editable changeset note or marker alone.
    Ok(())
}
fn admitted_page_state(
    reads: &[ReadDependency],
    path: &VaultRelativePath,
) -> Result<ExpectedState> {
    let mut proofs = reads.iter().filter(|read| &read.path == path);
    let proof = proofs.next().ok_or_else(|| {
        WikiError::new(
            ErrorCode::FreshnessConflict,
            "research update lacks an admitted page proof",
        )
    })?;
    if proofs.next().is_some() || !matches!(proof.expected, ExpectedState::Hash(_)) {
        return Err(invalid(
            "research admitted page proof is ambiguous or absent",
        ));
    }
    Ok(proof.expected.clone())
}
/// Only an actual committed proposal from this paid synthesis may advance an
/// old page proof. This authenticates retained payloads and current after bytes;
/// it never mutates the admitted task or refreshes arbitrary current hashes.
pub(crate) fn publication_substitutions(
    fs: &VaultFs,
    inspection: &LedgerInspection,
    key: &Blake3Hash,
) -> Result<BTreeMap<VaultRelativePath, (ExpectedState, ExpectedState)>> {
    let research = inspection
        .research
        .as_ref()
        .ok_or_else(|| invalid("research publication requires genesis"))?;
    let task = inspection
        .tasks
        .get(key)
        .ok_or_else(|| invalid("research synthesis missing"))?;
    if !research.active_tasks.contains(key)
        || task.state != TaskState::Completed
        || task.spec.stage != TaskStage::Synthesize
        || task.spec.capability != Some(Capability::Generate)
    {
        return Err(WikiError::new(
            ErrorCode::FreshnessConflict,
            "research synthesis authority retired",
        ));
    }
    let scope = publication_scope(fs, inspection)?;
    let engine = ChangeEngine::new(fs.clone())?;
    let ids = engine.change_ids()?;
    if ids.len() > 65_536 {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "research retained change scan exceeds ceiling",
        ));
    }
    let view = SourceView::from_fs_bounded(fs, 64 * 1024 * 1024, 4096)?;
    let mut substitutions = BTreeMap::new();
    for reference in &task.outputs {
        let stage = owned_output(fs, inspection, &task.spec, reference)?;
        let synthesis: synthesis::Synthesis = serde_json::from_value(stage.response)
            .map_err(|_| invalid("synthesis output schema differs"))?;
        let (_, admitted_reads, owner) = synthesis_binding(fs, inspection, &scope, &synthesis)?;
        if &owner != key {
            return Err(invalid("research synthesis publication owner is ambiguous"));
        }
        let mut seen = BTreeSet::new();
        for id in &ids {
            let actual = engine.inspect(id)?;
            let Some(marker) = actual.manifest.allocated_ids.get(MARKER) else {
                continue;
            };
            let Some(proposal) = synthesis.proposed_changes.iter().find(|proposal| {
                proposal_marker(&inspection.spec.run_id, proposal).as_ref() == Ok(marker)
            }) else {
                continue;
            };
            if !seen.insert(marker.clone()) {
                return Err(invalid("duplicate research proposal allocation"));
            }
            if actual.status != ChangeStatus::Committed {
                continue;
            }
            let reads = citation_dependencies(&view, proposal_citations(proposal))?;
            verify_page_change(
                fs,
                &engine,
                &actual,
                &inspection.spec.run_id,
                proposal,
                &reads,
                &admitted_reads,
            )?;
            let op = &actual.manifest.operations[0];
            if observed_state(fs, &op.target)? != op.after {
                return Err(WikiError::new(
                    ErrorCode::FreshnessConflict,
                    "committed research page changed after apply",
                ));
            }
            if substitutions
                .insert(op.target.clone(), (op.before.clone(), op.after.clone()))
                .is_some()
            {
                return Err(invalid("research committed page substitutions conflict"));
            }
        }
    }
    Ok(substitutions)
}
#[allow(clippy::too_many_arguments)]
fn stage_proposal(
    fs: &VaultFs,
    ledger: &JobLedger,
    inspection: &LedgerInspection,
    synthesis_key: &Blake3Hash,
    proposal: &ResearchProposal,
    reads: Vec<ReadDependency>,
    admitted_reads: &[ReadDependency],
    apply: bool,
    warnings: &mut Vec<String>,
) -> Result<PreparedChange> {
    let engine = ChangeEngine::new(fs.clone())?;
    let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(5))?;
    let marker = proposal_marker(&inspection.spec.run_id, proposal)?;
    let ids = engine.change_ids()?;
    if ids.len() > 65_536 {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "research retained change scan exceeds ceiling",
        ));
    }
    let mut existing = None;
    for id in ids {
        let actual = engine.inspect(&id)?;
        if actual.manifest.allocated_ids.get(MARKER) == Some(&marker) {
            verify_page_change(
                fs,
                &engine,
                &actual,
                &inspection.spec.run_id,
                proposal,
                &reads,
                admitted_reads,
            )?;
            if existing.replace(actual).is_some() {
                return Err(invalid("duplicate research proposal allocation"));
            }
        }
    }
    let actual = if let Some(existing) = existing {
        existing
    } else {
        let (page, path, before) = match proposal {
            ResearchProposal::CreatePage { .. } => {
                let (page, path) = create_choice(&inspection.spec.run_id, proposal)?;
                (page, path, None)
            }
            ResearchProposal::UpdatePage { record, .. } => {
                let (path, bytes) = current_page(fs, &inspection.spec.vault_id, record)?;
                if admitted_page_state(admitted_reads, &path)?
                    != ExpectedState::Hash(Blake3Hash::digest(&bytes))
                {
                    return Err(WikiError::new(
                        ErrorCode::FreshnessConflict,
                        "research update page changed after synthesis admission",
                    ));
                }
                (record.record_id.clone(), path, Some(bytes))
            }
        };
        let expected = match proposal {
            ResearchProposal::CreatePage { .. } => ExpectedState::Absent,
            ResearchProposal::UpdatePage { .. } => admitted_page_state(admitted_reads, &path)?,
        };
        let proposed = page_bytes(proposal, &page, before.as_deref())?;
        let draft = ChangeDraft {
            title: "Stage unassessed research page proposal".into(),
            origin: None,
            inverse_of: None,
            allocated_ids: BTreeMap::from([(MARKER.into(), marker), ("page".into(), page)]),
            read_preconditions: reads,
            operations: vec![ExpectedWrite {
                target: path,
                expected,
                proposed: Some(proposed),
                apply_after: vec![],
            }],
        };
        engine.prepare(&writer, draft)?
    };
    if actual.status == ChangeStatus::Committed {
        warnings.push(format!(
            "Research proposal {} was already committed; its retained identity is reused.",
            actual.prepared.change_id
        ));
    } else if change_stale(fs, &actual)? {
        warnings.push(format!(
            "Research proposal {} has changed target bytes and requires explicit resolution.",
            actual.prepared.change_id
        ));
    } else if apply {
        let catalog = Catalog::new(fs.clone(), inspection.spec.vault_id.clone());
        let epoch = inspection
            .research
            .as_ref()
            .ok_or_else(|| invalid("research publication requires epoch"))?
            .binding
            .number;
        ledger.with_research_publication(synthesis_key, epoch, || {
            engine.apply(&writer, &actual.prepared, &CatalogGraphValidator, &catalog)
        })?;
        warnings.push(format!(
            "Research proposal {} was already committed; its retained identity is reused.",
            actual.prepared.change_id
        ));
    } else if actual.observations.iter().any(|o| o.observed != o.before) {
        warnings.push(format!(
            "Research proposal {} has changed target bytes and requires explicit resolution.",
            actual.prepared.change_id
        ));
    }
    Ok(actual.prepared)
}

#[allow(clippy::too_many_arguments)]
pub fn build(
    fs: &VaultFs,
    ledger: &JobLedger,
    scope: &ResearchScope,
    passages: &[ResearchPassage],
    gaps: &[ResearchGap],
    synthesis: Option<ValidatedSynthesis>,
    changes: Vec<PreparedChange>,
    stop_reason: &str,
    partial: bool,
) -> Result<ResearchReport> {
    plan::validate_scope(scope)?;
    frontier::text(stop_reason, 4096, false)?;
    let inspection = ledger.inspect()?;
    let (bound_fs, _, _, options) = ledger.dispatcher_bindings();
    if bound_fs.root().path() != fs.root().path()
        || inspection.spec.scope.question.as_deref() != Some(&scope.question)
        || inspection.spec.scope.scope_payload_hash.as_ref()
            != Some(&Blake3Hash::digest(canonical_json(scope)?))
    {
        return Err(invalid("research report run/scope differs"));
    }
    let view = SourceView::from_fs_bounded(fs, 64 * 1024 * 1024, 4096)?;
    let mut report = ResearchReport { version: 1, run_id: inspection.spec.run_id.clone(), question: scope.question.clone(), partial, stop_reason: stop_reason.into(), passages: vec![], gaps: gaps.iter().take(scope.limits.stage.max_strings).cloned().collect(), synthesis: None, claim_assessments: vec![], proposed_changes: vec![], warnings: vec!["All generated claims are unassessed. Verified citations establish provenance, never entailment or acceptance.".into()] };
    if gaps.len() > report.gaps.len() {
        report
            .warnings
            .push("Research gaps exceeded the bounded report limit.".into());
    }
    let mut seen = BTreeSet::new();
    let mut quote_bytes = 0usize;
    for passage in passages.iter().take(scope.limits.stage.max_citations) {
        if !seen.insert(canonical_json(&passage.citation)?) {
            continue;
        }
        match view.verify(&passage.citation, CitationScope::Current) {
            Ok(verified) if verified.quote == passage.quote.as_bytes() => {
                quote_bytes = quote_bytes
                    .checked_add(verified.quote.len())
                    .ok_or_else(|| invalid("report quotation byte overflow"))?;
                if quote_bytes > scope.limits.stage.max_text_bytes {
                    report
                        .warnings
                        .push("Research quotations exceeded the bounded report limit.".into());
                    break;
                }
                report.passages.push(ResearchPassage {
                    citation: passage.citation.clone(),
                    quote: passage.quote.clone(),
                    dependencies: verified.dependencies,
                });
            }
            _ => {
                report.partial = true;
                report.gaps.push(gap(
                    "A supplied passage is no longer a current exact quotation and was omitted."
                        .into(),
                    "stale_passage",
                ));
            }
        }
    }
    let engine = ChangeEngine::new(fs.clone())?;
    if changes.len() > crate::changes::prepare::MAX_OPS {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "research prepared change count exceeds ceiling",
        ));
    }
    for change in changes {
        let actual = inspect_change(&engine, &change)?;
        if change_stale(fs, &actual)? {
            report.partial = true;
            report.warnings.push(format!("Retained proposal {} conflicts with current bytes; its original apply expectations remain unchanged.", change.change_id));
        }
        if !report.proposed_changes.contains(&change) {
            report.proposed_changes.push(change);
        }
    }
    if let Some(validated) = synthesis {
        let (binding, admitted_reads, synthesis_key) =
            synthesis_binding(fs, &inspection, scope, &validated.output)?;
        let mut output = validated.output;
        for record in &binding.current_records {
            if record.vault_id != inspection.spec.vault_id {
                return Err(invalid("synthesis whitelist contains another vault"));
            }
        }
        for section in &mut output.sections {
            section.claims.retain(|claim| {
                let current = claim.citations.iter().all(|c| binding.citations.contains(c) && view.verify(c, CitationScope::Current).is_ok());
                if !current { report.partial = true; report.gaps.push(gap(format!("Unassessed claim withheld because its cited provenance is unavailable: {}", claim.text), "stale_claim")); }
                current
            });
        }
        let mut proposals = Vec::new();
        for proposal in std::mem::take(&mut output.proposed_changes) {
            if let ResearchProposal::UpdatePage { record, .. } = &proposal
                && (record.expected_kind != RecordKind::Page
                    || !binding.current_records.contains(record))
            {
                return Err(invalid(
                    "research update page was not authorized by its synthesis input",
                ));
            }
            if proposal_citations(&proposal).iter().any(|c| {
                !binding.citations.contains(c) || view.verify(c, CitationScope::Current).is_err()
            }) {
                report.partial = true;
                report.gaps.push(gap("An unassessed page proposal was withheld because its cited provenance is unavailable.".into(), "stale_proposal"));
                continue;
            }
            proposals.push(proposal);
        }
        output.proposed_changes = proposals;
        let allowed: Vec<_> = binding
            .citations
            .iter()
            .filter(|c| view.verify(c, CitationScope::Current).is_ok())
            .cloned()
            .collect();
        let checked = synthesis::validate(
            &canonical_json(&output)?,
            &scope.limits.stage,
            &allowed,
            &binding.current_records,
            &view,
        )?;
        report.claim_assessments = checked.claim_assessments;
        for message in checked.gaps {
            report.gaps.push(gap(message, "unassessed_claim"));
        }
        for proposal in &checked.output.proposed_changes {
            if options.policy.dry_run {
                report.warnings.push(
                    "Dry-run page proposal was validated without staging or applying it.".into(),
                );
                continue;
            }
            let reads = citation_dependencies(&view, proposal_citations(proposal))?;
            match stage_proposal(
                fs,
                ledger,
                &inspection,
                &synthesis_key,
                proposal,
                reads,
                &admitted_reads,
                scope.apply,
                &mut report.warnings,
            ) {
                Ok(change) => {
                    if change_stale(fs, &inspect_change(&engine, &change)?)? {
                        report.partial = true;
                        report.warnings.push(format!("Retained proposal {} conflicts with current bytes; its original apply expectations remain unchanged.", change.change_id));
                    }
                    if !report.proposed_changes.contains(&change) {
                        report.proposed_changes.push(change);
                    }
                }
                Err(error) => {
                    report.partial = true;
                    report.gaps.push(gap(
                        format!("Unassessed page proposal remains unapplied: {}", error.code),
                        "proposal_conflict",
                    ));
                }
            }
        }
        report.synthesis = Some(checked.output);
    }
    // The caller can request apply only for the generated page drafts authenticated
    // above. Prior extraction changes remain staged references, never acceptance.
    if report.gaps.len() > scope.limits.stage.max_strings {
        report.warnings.push("Additional unsupported passages/proposals remain in immutable stage history because the report gap limit was reached.".into());
        // A withheld claim's only report representation is its explicit gap;
        // preserve those before redundant passage and operational diagnostics.
        report.gaps.sort_by_key(|gap| match gap.code.as_str() {
            "stale_claim" => 0,
            "stale_proposal" => 1,
            "unassessed_claim" => 2,
            _ => 3,
        });
        report.gaps.truncate(scope.limits.stage.max_strings);
    }
    report.warnings.sort();
    report.warnings.dedup();
    if canonical_json(&report)?.len() > scope.limits.stage.max_bytes {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "research report exceeds caller byte limit",
        ));
    }
    report_hash(&report)?;
    Ok(report)
}
fn admission_id(key: &Blake3Hash) -> Result<Blake3Hash> {
    Ok(Blake3Hash::digest(canonical_json(&(
        "lwiki.research-report-admission.v1",
        key,
    ))?))
}
fn decode_report(
    fs: &VaultFs,
    inspection: &LedgerInspection,
    task: &TaskInspection,
    reference: &DurableOutputRef,
) -> Result<ResearchReport> {
    if task.spec.stage != TaskStage::StageChanges
        || task.spec.capability.is_some()
        || task.state != TaskState::Completed
    {
        return Err(invalid(
            "research report task is not completed local StageChanges",
        ));
    }
    let bytes = task_bytes(fs, &task.spec, &inspection.spec.run_id)?;
    let report: ResearchReport = crate::changes::prepare::strict_json(&bytes)?;
    if canonical_json(&report)? != bytes
        || report.version != 1
        || report.run_id != inspection.spec.run_id
        || inspection.spec.scope.question.as_deref() != Some(&report.question)
    {
        return Err(invalid("research report descriptor scope differs"));
    }
    let output = owned_output(fs, inspection, &task.spec, reference)?;
    if output.attempt.is_some()
        || canonical_json(&output.response)? != bytes
        || output.response_hash != report_hash(&report)?
    {
        return Err(invalid("research report output and descriptor differ"));
    }
    if report
        .claim_assessments
        .iter()
        .any(|a| a.status != ClaimStatus::Unassessed)
    {
        return Err(invalid("research report claim status differs"));
    }
    let engine = ChangeEngine::new(fs.clone())?;
    for change in &report.proposed_changes {
        inspect_change(&engine, change)?;
    }
    Ok(report)
}
/// Latest authenticated report by actual frontier-admission event sequence.
pub fn latest(fs: &VaultFs, inspection: &LedgerInspection) -> Result<Option<ResearchReport>> {
    let Some(research) = &inspection.research else {
        return Ok(None);
    };
    let mut newest = None;
    for task in inspection.tasks.values() {
        if task.spec.stage != TaskStage::StageChanges || task.state != TaskState::Completed {
            continue;
        }
        let Some(transition) = research.transitions.get(&admission_id(&task.spec.key)?) else {
            return Err(invalid("research report admission event missing"));
        };
        for reference in &task.outputs {
            let report = decode_report(fs, inspection, task, reference)?;
            if newest
                .as_ref()
                .is_none_or(|(sequence, _)| transition.event.sequence > *sequence)
            {
                newest = Some((transition.event.sequence, report));
            }
        }
    }
    Ok(newest.map(|(_, report)| report))
}
fn immutable(
    fs: &VaultFs,
    writer: &WriterPermit,
    path: &VaultRelativePath,
    bytes: &[u8],
) -> Result<()> {
    if let Some(existing) = read_bounded(fs, path, MAX_BYTES)? {
        if existing != bytes {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "immutable research report descriptor differs",
            ));
        }
        return Ok(());
    }
    let parent = path
        .as_str()
        .rsplit_once('/')
        .ok_or_else(|| invalid("report descriptor parent missing"))?
        .0;
    if fs.ensure_directory(&VaultRelativePath::new(parent)?, writer)? == DirectorySync::Unsupported
    {
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "report directory durability unavailable",
        ));
    }
    let staged = fs.stage(path, bytes, writer)?;
    if fs.replace(staged, &ExpectedState::Absent, writer)? == DirectorySync::Unsupported {
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "report descriptor durability unavailable",
        ));
    }
    Ok(())
}
fn publication_scope(fs: &VaultFs, inspection: &LedgerInspection) -> Result<ResearchScope> {
    let reference = &inspection
        .spec
        .scope
        .research
        .as_ref()
        .ok_or_else(|| invalid("report requires research genesis"))?
        .scope;
    let bytes = read_bounded(fs, &reference.path, MAX_BYTES)?
        .ok_or_else(|| invalid("report scope missing"))?;
    if bytes.len() as u64 != reference.byte_len || Blake3Hash::digest(&bytes) != reference.hash {
        return Err(invalid("report scope bytes differ"));
    }
    let scope: ResearchScope = crate::changes::prepare::strict_json(&bytes)?;
    if canonical_json(&scope)? != bytes {
        return Err(invalid("report scope is not canonical"));
    }
    plan::validate_scope(&scope)?;
    Ok(scope)
}
fn authenticate_projection(
    fs: &VaultFs,
    inspection: &LedgerInspection,
    scope: &ResearchScope,
    report: &ResearchReport,
) -> Result<Option<Vec<ReadDependency>>> {
    let Some(projected) = &report.synthesis else {
        return if report.claim_assessments.is_empty() {
            Ok(None)
        } else {
            Err(invalid("report assessments have no synthesis"))
        };
    };
    for task in inspection.tasks.values().filter(|task| {
        task.spec.stage == TaskStage::Synthesize && task.state == TaskState::Completed
    }) {
        for reference in &task.outputs {
            let output = owned_output(fs, inspection, &task.spec, reference)?;
            let original: synthesis::Synthesis = serde_json::from_value(output.response)
                .map_err(|_| invalid("synthesis output schema differs"))?;
            if original.sections.len() != projected.sections.len()
                || original.unanswered_questions != projected.unanswered_questions
            {
                continue;
            }
            let included =
                projected
                    .sections
                    .iter()
                    .zip(&original.sections)
                    .all(|(section, source)| {
                        if section.heading != source.heading {
                            return false;
                        }
                        let mut remaining = source.claims.iter();
                        section
                            .claims
                            .iter()
                            .all(|claim| remaining.any(|candidate| candidate == claim))
                    })
                    && projected
                        .proposed_changes
                        .iter()
                        .all(|proposal| original.proposed_changes.contains(proposal));
            if !included {
                continue;
            }
            let (binding, admitted_reads, _) = synthesis_binding(fs, inspection, scope, &original)?;
            if projected.proposed_changes.iter().any(|proposal| matches!(proposal,
                ResearchProposal::UpdatePage { record, .. } if record.vault_id != inspection.spec.vault_id || record.expected_kind != RecordKind::Page || !binding.current_records.contains(record))) {
                return Err(invalid("report projected update was not authorized"));
            }
            let citations: Vec<_> = projected
                .sections
                .iter()
                .flat_map(|s| s.claims.iter().flat_map(|c| &c.citations))
                .chain(
                    projected
                        .proposed_changes
                        .iter()
                        .flat_map(proposal_citations),
                )
                .collect();
            if citations
                .iter()
                .any(|citation| !binding.citations.contains(citation))
            {
                return Err(invalid("report projected citation was not supplied"));
            }
            return Ok(Some(admitted_reads));
        }
    }
    Err(invalid(
        "report synthesis projection has no authenticated paid origin",
    ))
}
fn report_read_dependencies(
    fs: &VaultFs,
    inspection: &LedgerInspection,
    report: &ResearchReport,
) -> Result<Vec<ReadDependency>> {
    if report.version != 1
        || report.run_id != inspection.spec.run_id
        || inspection.spec.scope.question.as_deref() != Some(&report.question)
    {
        return Err(invalid("research report publication binding differs"));
    }
    frontier::text(&report.stop_reason, 4096, false)?;
    let claim_count = report.synthesis.as_ref().map_or(0, |s| {
        s.sections.iter().map(|section| section.claims.len()).sum()
    });
    if report.claim_assessments.len() != claim_count
        || report
            .claim_assessments
            .iter()
            .any(|a| a.status != ClaimStatus::Unassessed)
    {
        return Err(invalid(
            "research report claim assessment count or status differs",
        ));
    }
    let scope = publication_scope(fs, inspection)?;
    let admitted_reads = authenticate_projection(fs, inspection, &scope, report)?;
    report_hash(report)?;
    let mut reads = BTreeMap::new();
    let view = SourceView::from_fs_bounded(fs, 64 * 1024 * 1024, 4096)?;
    for passage in &report.passages {
        let proof = view.verify(&passage.citation, CitationScope::Current)?;
        if proof.quote != passage.quote.as_bytes() || proof.dependencies != passage.dependencies {
            return Err(invalid("research report passage proof differs"));
        }
        append_dependencies(&mut reads, &proof.dependencies)?;
    }
    if let Some(synthesis) = &report.synthesis {
        for (section_index, section) in synthesis.sections.iter().enumerate() {
            for (claim_index, claim) in section.claims.iter().enumerate() {
                let assessment = report
                    .claim_assessments
                    .iter()
                    .find(|a| a.section_index == section_index && a.claim_index == claim_index)
                    .ok_or_else(|| invalid("research report claim assessment missing"))?;
                if assessment.status != ClaimStatus::Unassessed
                    || assessment.provenance_verified == claim.citations.is_empty()
                {
                    return Err(invalid("research report claim provenance differs"));
                }
                append_dependencies(&mut reads, &citation_dependencies(&view, &claim.citations)?)?;
            }
        }
    }
    let engine = ChangeEngine::new(fs.clone())?;
    for change in &report.proposed_changes {
        let actual = inspect_change(&engine, change)?;
        if let Some(marker) = actual.manifest.allocated_ids.get(MARKER) {
            let proposal = report
                .synthesis
                .as_ref()
                .into_iter()
                .flat_map(|s| &s.proposed_changes)
                .find(|proposal| {
                    proposal_marker(&inspection.spec.run_id, proposal).as_ref() == Ok(marker)
                })
                .ok_or_else(|| invalid("report prepared page change has no projected proposal"))?;
            let proposal_reads = citation_dependencies(&view, proposal_citations(proposal))?;
            verify_page_change(
                fs,
                &engine,
                &actual,
                &inspection.spec.run_id,
                proposal,
                &proposal_reads,
                admitted_reads.as_deref().unwrap_or(&[]),
            )?;
        }
        for read in &actual.manifest.read_preconditions {
            append_dependencies(
                &mut reads,
                &[ReadDependency {
                    path: read.path.clone(),
                    expected: observed_state(fs, &read.path)?,
                }],
            )?;
        }
        for op in &actual.manifest.operations {
            append_dependencies(
                &mut reads,
                &[ReadDependency {
                    path: op.target.clone(),
                    expected: observed_state(fs, &op.target)?,
                }],
            )?;
        }
    }
    Ok(dependencies(reads))
}
/// Authenticate report controls without ledger recursion or writes.
pub(crate) fn authenticate_local_report(
    fs: &VaultFs,
    inspection: &LedgerInspection,
    report: &ResearchReport,
) -> Result<Vec<ReadDependency>> {
    report_read_dependencies(fs, inspection, report)
}
pub fn publish(
    fs: &VaultFs,
    ledger: &JobLedger,
    report: &ResearchReport,
) -> Result<DurableOutputRef> {
    let inspection = ledger.inspect()?;
    let (bound_fs, _, _, options) = ledger.dispatcher_bindings();
    if options.policy.dry_run {
        return Err(WikiError::new(
            ErrorCode::OfflineUnavailable,
            "dry-run report publication prohibited",
        ));
    }
    if bound_fs.root().path() != fs.root().path()
        || report.version != 1
        || report.run_id != inspection.spec.run_id
        || inspection.spec.scope.question.as_deref() != Some(&report.question)
    {
        return Err(invalid("research report publication binding differs"));
    }
    let reads = report_read_dependencies(fs, &inspection, report)?;
    let hash = report_hash(report)?;
    let bytes = canonical_json(report)?;
    for task in inspection
        .tasks
        .values()
        .filter(|t| t.spec.stage == TaskStage::StageChanges && t.state == TaskState::Completed)
    {
        for reference in &task.outputs {
            let previous = decode_report(fs, &inspection, task, reference)?;
            if canonical_json(&previous)? == bytes {
                return Ok(reference.clone());
            }
        }
    }
    let mut task = TaskSpec {
        key: Blake3Hash::digest([]),
        stage: TaskStage::StageChanges,
        capability: None,
        priority: i32::MAX,
        dependencies: vec![],
        input_hash: hash.clone(),
        prompt_hash: None,
        schema_hash: None,
        model_hash: None,
        settings_hash: Blake3Hash::digest(b"lwiki.research-report.v1"),
        source_bindings: reads,
        input: BoundedPayloadRef {
            path: VaultRelativePath::new(format!(
                "runs/{}/inputs/{}.json",
                report.run_id,
                hash.hex()
            ))?,
            hash,
            byte_len: bytes.len() as u64,
        },
    };
    task.key = crate::jobs::tasks::task_key(&task)?;
    let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(5))?;
    immutable(fs, &writer, &task.input.path, &bytes)?;
    drop(writer);
    if !inspection.tasks.contains_key(&task.key) {
        let research = inspection
            .research
            .as_ref()
            .ok_or_else(|| invalid("research report requires research genesis"))?;
        ledger.admit_research_frontier(EventPayload::ResearchFrontierAdmitted {
            version: 1,
            epoch: research.binding.number,
            prior_revision: research.frontier_revision,
            admission_id: admission_id(&task.key)?,
            round: 0,
            origins: vec![],
            tasks: vec![task.clone()],
            task_origins: vec![],
            parent_outputs: vec![],
        })?;
    }
    stages::publish_local(
        fs,
        ledger,
        &task,
        serde_json::to_value(report).map_err(|_| invalid("research report encoding"))?,
        task.source_bindings.clone(),
        inspection.spec.created_at_utc_ms,
    )
}

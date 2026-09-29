//! Bounded acquisition helpers; research scheduling/synthesis belongs to P20.
use crate::{
    catalog::{Catalog, CatalogGraphValidator},
    changes::ChangeEngine,
    domain::*,
    graph::packet::canonical_json,
    jobs::*,
    providers::{
        dispatcher::Dispatcher,
        public_fetch::*,
        search_wire::SearchLead,
        types::{RemoteInput, RemoteOperation},
    },
    sources::{
        CaptureRequest, ExtractionInput, SourceOrigin, SourcePlan, SourceStore,
        web_normalize::{self, WebGap},
    },
    vault::{ExpectedState, VaultFs, WriterPermit},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeadOrigins {
    pub lead: SearchLead,
    pub origins: Vec<String>,
}
pub fn deduplicate_leads(
    pages: impl IntoIterator<Item = (String, Vec<SearchLead>)>,
) -> Result<Vec<LeadOrigins>> {
    let mut result = Vec::<LeadOrigins>::new();
    let mut keys = BTreeMap::<String, usize>::new();
    for (origin, leads) in pages {
        for lead in leads {
            let key = validate_url(&lead.url, None)?.to_string();
            if let Some(index) = keys.get(&key) {
                if !result[*index].origins.contains(&origin) {
                    result[*index].origins.push(origin.clone());
                }
            } else {
                keys.insert(key, result.len());
                result.push(LeadOrigins {
                    lead,
                    origins: vec![origin.clone()],
                });
            }
        }
    }
    Ok(result)
}
/// Content grouping never merges identities or loses origin URLs.
pub fn group_content_origins(captures: &[FetchCapture]) -> BTreeMap<Blake3Hash, Vec<String>> {
    let mut groups = BTreeMap::<Blake3Hash, Vec<String>>::new();
    for capture in captures {
        let origins = groups.entry(capture.original_hash.clone()).or_default();
        if !origins.contains(&capture.observed_url) {
            origins.push(capture.observed_url.clone());
        }
    }
    groups
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedirectObservation {
    pub observed_url: String,
    pub requested_url: String,
    pub status: u16,
    pub location: String,
    pub fetched_at_utc_ms: i64,
    pub original_hash: Blake3Hash,
    pub attempt: AttemptRef,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebProvenance {
    pub version: u32,
    pub observed_url: String,
    pub final_url: String,
    pub redirects: Vec<RedirectObservation>,
    pub fetched_at_utc_ms: i64,
    pub media_type: Option<String>,
    pub content_encoding: Option<String>,
    pub original_hash: Blake3Hash,
    pub normalizer: String,
    pub normalizer_fingerprint: Blake3Hash,
    pub gap: Option<WebGap>,
    pub attempt: AttemptRef,
}
pub struct CapturedSource {
    pub source_id: RecordId,
    pub revision_id: RecordId,
    pub gap: Option<WebGap>,
    pub provenance: WebProvenance,
}

pub fn plan_capture(
    store: &SourceStore,
    capture: &FetchCapture,
    original_url: &str,
    redirects: Vec<RedirectObservation>,
    attempt: AttemptRef,
) -> Result<(SourcePlan, WebProvenance)> {
    validate_url(original_url, None)?;
    validate_url(&capture.requested_url, Some(original_url))?;
    let mut expected = original_url.to_owned();
    for hop in &redirects {
        if hop.observed_url != expected
            || validate_url(&hop.observed_url, None)?.as_str() != hop.requested_url
            || !matches!(hop.status, 301 | 302 | 303 | 307 | 308)
        {
            return Err(WikiError::invalid("redirect provenance chain differs"));
        }
        let joined = url::Url::parse(&hop.requested_url)
            .map_err(|_| WikiError::invalid("redirect provenance URL invalid"))?
            .join(&hop.location)
            .map_err(|_| WikiError::invalid("redirect provenance Location invalid"))?;
        expected = validate_url(joined.as_str(), Some(&hop.requested_url))?.to_string();
    }
    if expected != capture.observed_url {
        return Err(WikiError::invalid(
            "capture origin is not observed redirect chain",
        ));
    }
    if redirects.len() > usize::from(capture.limits.redirects)
        || capture.original_hash != Blake3Hash::digest(&capture.original)
    {
        return Err(WikiError::invalid(
            "capture provenance or redirect cap invalid",
        ));
    }
    let normalized = web_normalize::normalize(capture);
    let provenance = WebProvenance {
        version: 1,
        observed_url: original_url.into(),
        final_url: capture.requested_url.clone(),
        redirects,
        fetched_at_utc_ms: capture.fetched_at_utc_ms,
        media_type: capture.header("content-type").map(str::to_owned),
        content_encoding: capture.header("content-encoding").map(str::to_owned),
        original_hash: capture.original_hash.clone(),
        normalizer: web_normalize::NORMALIZER.into(),
        normalizer_fingerprint: normalized.fingerprint.clone(),
        gap: normalized.gap,
        attempt,
    };
    let extraction = match normalized.content {
        Some(content) => ExtractionInput::Supplied {
            extractor: web_normalize::NORMALIZER.into(),
            fingerprint: normalized.fingerprint,
            content,
        },
        None => ExtractionInput::Unsupported {
            extractor: web_normalize::NORMALIZER.into(),
            fingerprint: normalized.fingerprint,
        },
    };
    let mut plan = store.plan_capture(CaptureRequest {
        title: format!("Captured {}", capture.requested_url),
        origin_kind: SourceOrigin::Url,
        origin: original_url.into(),
        original: capture.original.clone(),
        extraction,
        media_type: provenance.media_type.clone(),
    })?;
    let draft = plan
        .draft
        .as_mut()
        .ok_or_else(|| WikiError::invalid("new web capture draft absent"))?;
    let revision = draft
        .operations
        .iter_mut()
        .find(|op| op.target.as_str().ends_with("/revision.md"))
        .ok_or_else(|| WikiError::invalid("web revision manifest absent"))?;
    let bytes = revision
        .proposed
        .as_mut()
        .ok_or_else(|| WikiError::invalid("web revision manifest bytes absent"))?;
    bytes.extend_from_slice(b"# Acquisition provenance\n\n```lwiki-acquisition-v1\n");
    bytes.extend_from_slice(&canonical_json(&provenance)?);
    bytes.extend_from_slice(b"\n```\n");
    Ok((plan, provenance))
}
pub fn plan_task(
    fs: &VaultFs,
    url: &str,
    limits: FetchLimits,
    dependencies: Vec<Blake3Hash>,
) -> Result<TaskSpec> {
    validate_url(url, None)?;
    limits.validate()?;
    let input = RemoteInput {
        version: 1,
        operation: RemoteOperation::Fetch {
            url: url.into(),
            limits,
        },
    };
    let bytes = canonical_json(&input)?;
    let hash = Blake3Hash::digest(&bytes);
    let directory = VaultRelativePath::new(".wiki/state/acquisition-inputs")?;
    let path = VaultRelativePath::new(format!(
        "{directory}/{}.json",
        hash.as_str().trim_start_matches("blake3:")
    ))?;
    let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(5))?;
    fs.ensure_directory(&directory, &writer)?;
    if let Some(before) = fs.read_before(&path)? {
        if before.bytes != bytes {
            return Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                "acquisition descriptor changed",
            ));
        }
    } else {
        let staged = fs.stage(&path, &bytes, &writer)?;
        fs.replace(staged, &ExpectedState::Absent, &writer)?;
    }
    drop(writer);
    let mut task = TaskSpec {
        key: Blake3Hash::digest([]),
        stage: TaskStage::Capture,
        capability: Some(Capability::Fetch),
        priority: 0,
        dependencies,
        input_hash: input_fingerprint(&input)?,
        prompt_hash: None,
        schema_hash: None,
        model_hash: None,
        settings_hash: settings_fingerprint(),
        source_bindings: vec![],
        input: BoundedPayloadRef {
            path,
            hash,
            byte_len: bytes.len() as u64,
        },
    };
    task.key = crate::jobs::tasks::task_key(&task)?;
    Ok(task)
}
fn materialize(
    fs: &VaultFs,
    ledger: &JobLedger,
    mut plan: MaterializationPlan,
    source: Option<SourcePlan>,
) -> Result<()> {
    let inspection = ledger.inspect()?;
    let vault_id = inspection.spec.vault_id;
    let mut outputs = Vec::new();
    if let Some(source) = source
        && let Some(draft) = source.draft
    {
        for op in &draft.operations {
            if op.target.as_str().ends_with("/source.md")
                || op.target.as_str().ends_with("/revision.md")
            {
                let kind = if op.target.as_str().ends_with("/source.md") {
                    RecordKind::Source
                } else {
                    RecordKind::Revision
                };
                let record_id = if kind == RecordKind::Source {
                    source.source_id.clone()
                } else {
                    source.revision_id.clone()
                };
                outputs.push(DurableOutputRef {
                    record: RecordRef {
                        vault_id: vault_id.clone(),
                        record_id,
                        expected_kind: kind,
                    },
                    path: op.target.clone(),
                    hash: Blake3Hash::digest(
                        op.proposed
                            .as_ref()
                            .ok_or_else(|| WikiError::invalid("capture output bytes absent"))?,
                    ),
                });
            }
        }
        plan = crate::jobs::checkpoint::receipt_plan(
            ledger,
            &plan.attempt,
            OutputDisposition::Validated,
            outputs.clone(),
            vec![],
            draft.operations,
        )?;
        plan.draft.allocated_ids.extend(draft.allocated_ids);
        plan.draft
            .read_preconditions
            .extend(draft.read_preconditions);
    }
    let receipt_op = plan
        .draft
        .operations
        .last()
        .ok_or_else(|| WikiError::invalid("acquisition receipt missing"))?;
    let receipt = DurableOutputRef {
        record: RecordRef {
            vault_id: vault_id.clone(),
            record_id: plan.receipt.receipt_id.clone(),
            expected_kind: RecordKind::RunEvent,
        },
        path: receipt_op.target.clone(),
        hash: Blake3Hash::digest(
            receipt_op
                .proposed
                .as_ref()
                .ok_or_else(|| WikiError::invalid("receipt bytes absent"))?,
        ),
    };
    let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(5))?;
    let engine = ChangeEngine::new(fs.clone())?;
    let catalog = Catalog::new(fs.clone(), vault_id);
    let prepared = engine.prepare(&writer, plan.draft)?.prepared;
    engine.apply(&writer, &prepared, &CatalogGraphValidator, &catalog)?;
    drop(writer);
    ledger.outputs_committed(&plan.attempt, &prepared, receipt, outputs, vec![])?;
    ledger.settle(&plan.attempt)?;
    Ok(())
}
pub fn settle_receipt(fs: &VaultFs, ledger: &JobLedger, plan: MaterializationPlan) -> Result<()> {
    materialize(fs, ledger, plan, None)
}
fn settle_validated_redirect(fs: &VaultFs, ledger: &JobLedger, attempt: &AttemptRef) -> Result<()> {
    // A validated, complete public redirect is a durable acquisition output even
    // though it has no extracted source passage. Completing this hop permits its
    // separately admitted dependent destination; unknown/failed hops cannot.
    let retained = retained_capture(ledger, attempt)?;
    if retained.redirect()?.is_none() {
        return Err(WikiError::invalid("validated redirect required"));
    }
    let plan = crate::jobs::checkpoint::receipt_plan(
        ledger,
        attempt,
        OutputDisposition::Validated,
        vec![],
        vec![],
        vec![],
    )?;
    materialize(fs, ledger, plan, None)
}
pub fn capture_outcome(
    fs: &VaultFs,
    ledger: &JobLedger,
    outcome: PublicFetchOutcome,
    original_url: &str,
    redirects: Vec<RedirectObservation>,
) -> Result<CapturedSource> {
    // Enforce the real durable response before decompression/normalization.
    let inspection = ledger.inspect()?;
    let actual = inspection
        .attempts
        .iter()
        .find(|a| a.attempt == outcome.attempt)
        .ok_or_else(|| WikiError::invalid("captured attempt absent"))?;
    if actual.spool.as_ref() != Some(&outcome.spool)
        || outcome.materialization.attempt != outcome.attempt
        || outcome.spool.response.hash != outcome.capture.original_hash
    {
        return Err(WikiError::invalid("raw response spool binding differs"));
    }
    let raw =
        crate::changes::prepare::read_bounded(fs, &outcome.spool.response.path, 4 * 1024 * 1024)?
            .ok_or_else(|| {
            WikiError::new(ErrorCode::RecoveryRequired, "raw acquisition spool missing")
        })?;
    if raw != outcome.capture.original || Blake3Hash::digest(raw) != outcome.capture.original_hash {
        return Err(WikiError::new(
            ErrorCode::RecoveryRequired,
            "raw acquisition spool changed",
        ));
    }
    let retained = retained_capture(ledger, &outcome.attempt)?;
    if PublicCaptureMetadata::from_capture(&retained)
        != PublicCaptureMetadata::from_capture(&outcome.capture)
    {
        return Err(WikiError::new(
            ErrorCode::RecoveryRequired,
            "capture provenance differs from protected spool",
        ));
    }
    for hop in &redirects {
        let observed = retained_capture(ledger, &hop.attempt)?;
        if observed.observed_url != hop.observed_url
            || observed.requested_url != hop.requested_url
            || observed.status != hop.status
            || observed.fetched_at_utc_ms != hop.fetched_at_utc_ms
            || observed.original_hash != hop.original_hash
            || observed.header("location") != Some(hop.location.as_str())
        {
            return Err(WikiError::new(
                ErrorCode::RecoveryRequired,
                "redirect provenance differs from protected spool",
            ));
        }
    }
    let store = SourceStore::new(fs.clone());
    let (plan, provenance) = plan_capture(
        &store,
        &outcome.capture,
        original_url,
        redirects,
        outcome.attempt,
    )?;
    let result = CapturedSource {
        source_id: plan.source_id.clone(),
        revision_id: plan.revision_id.clone(),
        gap: provenance.gap.clone(),
        provenance,
    };
    materialize(fs, ledger, outcome.materialization, Some(plan))?;
    Ok(result)
}

pub enum AcquisitionResult {
    Captured(Box<CapturedSource>),
    Gap { kind: WebGap, url: String },
}
/// Each hop is a separately persisted task and separately admitted dispatcher call.
pub fn fetch_url(
    fs: &VaultFs,
    dispatcher: &Dispatcher,
    ledger: &JobLedger,
    url: &str,
    limits: FetchLimits,
    options: &PublicFetchOptions,
) -> Result<AcquisitionResult> {
    let (owner_fs, _, _, job_options) = ledger.dispatcher_bindings();
    if job_options.policy.offline || job_options.policy.dry_run {
        return Err(WikiError::new(
            ErrorCode::OfflineUnavailable,
            "acquisition prohibited by execution policy",
        ));
    }
    if job_options.cancel.is_cancelled() {
        return Err(WikiError::new(
            ErrorCode::Cancelled,
            "acquisition cancelled",
        ));
    }
    if owner_fs.root().path() != fs.root().path() {
        return Err(WikiError::new(
            ErrorCode::ProfileUntrusted,
            "acquisition vault differs from ledger",
        ));
    }
    let mut current = url.to_owned();
    let mut redirects = Vec::new();
    let mut seen = BTreeSet::new();
    let mut dependencies = vec![];
    loop {
        let key_url = validate_url(&current, Some(url))?.to_string();
        if !seen.insert(key_url) {
            return Ok(AcquisitionResult::Gap {
                kind: WebGap::RedirectRejected,
                url: current,
            });
        }
        let task = plan_task(fs, &current, limits.clone(), dependencies)?;
        let key = task.key.clone();
        ledger.add_tasks(vec![task])?;
        let outcome = match dispatcher.execute_public(ledger, &key, options) {
            Ok(outcome) => outcome,
            Err(mut failure) => {
                if let Some(plan) = failure.materialization.take() {
                    settle_receipt(fs, ledger, plan)?;
                }
                return Err(failure.error);
            }
        };
        let next = match outcome.capture.redirect() {
            Ok(next) => next,
            Err(_) => {
                settle_receipt(fs, ledger, outcome.materialization)?;
                return Ok(AcquisitionResult::Gap {
                    kind: WebGap::RedirectRejected,
                    url: current,
                });
            }
        };
        if let Some(next) = next {
            if redirects.len() >= usize::from(limits.redirects) {
                settle_validated_redirect(fs, ledger, &outcome.attempt)?;
                return Ok(AcquisitionResult::Gap {
                    kind: WebGap::RedirectLimit,
                    url: current,
                });
            }
            redirects.push(RedirectObservation {
                observed_url: outcome.capture.observed_url.clone(),
                requested_url: outcome.capture.requested_url.clone(),
                status: outcome.capture.status,
                location: outcome.capture.header("location").unwrap_or("").into(),
                fetched_at_utc_ms: outcome.capture.fetched_at_utc_ms,
                original_hash: outcome.capture.original_hash.clone(),
                attempt: outcome.attempt.clone(),
            });
            settle_validated_redirect(fs, ledger, &outcome.attempt)?;
            dependencies = vec![key];
            current = next;
        } else {
            return Ok(AcquisitionResult::Captured(Box::new(capture_outcome(
                fs, ledger, outcome, url, redirects,
            )?)));
        }
    }
}

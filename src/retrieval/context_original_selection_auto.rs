//! Deterministic whole-original admission from the existing bounded lexical order.
use super::*;

const MAX_HITS: usize = 10;
const MAX_CANDIDATES: usize = 80;

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    NotCaptured,
    Ineligible,
    DuplicateOwner,
    EmptyOriginal,
    OriginalBytes,
    IndexedBytes,
    SerializedInputBytes,
    Admitted,
}

#[derive(Serialize)]
struct Decision {
    rank: usize,
    locator: DocumentLocator,
    source_id: Option<RecordId>,
    source_revision: Option<RevisionId>,
    original_bytes: Option<usize>,
    indexed_bytes: Option<usize>,
    outcome: Outcome,
}

#[derive(Serialize)]
pub(super) struct Admission {
    policy: &'static str,
    discovery_candidate_count: usize,
    discovery_returned_hits: usize,
    discovery_omitted_candidates: usize,
    discovery_truncated: bool,
    max_hits: usize,
    max_candidates: usize,
    metadata_authority: &'static str,
    authenticated_paths: Vec<VaultRelativePath>,
    decisions: Vec<Decision>,
}

impl Admission {
    pub(super) fn authenticated_paths(&self) -> Vec<VaultRelativePath> {
        self.authenticated_paths.clone()
    }

    pub(super) fn omissions(&self) -> Vec<ContextOmission> {
        let mut omissions = self
            .decisions
            .iter()
            .filter(|decision| decision.outcome != Outcome::Admitted)
            .map(|decision| ContextOmission {
                record_id: decision.source_revision.clone(),
                path: Some(decision.locator.path.clone()),
                reason: match decision.outcome {
                    Outcome::NotCaptured => "original_discovery_not_captured",
                    Outcome::Ineligible => "original_discovery_ineligible",
                    Outcome::DuplicateOwner => "original_discovery_duplicate_owner",
                    Outcome::EmptyOriginal => "original_admission_empty_capture",
                    Outcome::OriginalBytes => "original_admission_raw_byte_limit",
                    Outcome::IndexedBytes => "original_admission_indexed_byte_limit",
                    Outcome::SerializedInputBytes => "original_admission_serialized_task_limit",
                    Outcome::Admitted => unreachable!("admitted originals have no omission"),
                }
                .into(),
                count: 1,
            })
            .collect::<Vec<_>>();
        if self.discovery_truncated || self.discovery_candidate_count > self.discovery_returned_hits
        {
            omissions.push(ContextOmission {
                record_id: None,
                path: None,
                reason: "original_discovery_bounded_candidates_not_complete".into(),
                count: self
                    .discovery_candidate_count
                    .saturating_sub(self.discovery_returned_hits)
                    .max(self.discovery_omitted_candidates)
                    .max(1),
            });
        }
        omissions
    }

    pub(super) fn warnings(&self) -> Vec<String> {
        vec![
            "Automatic originals preserve bounded lexical order without refilling omitted owners. Omitted owner metadata comes from the pinned catalog and its canonical bytes are not claimed verified unless listed in authenticated_paths. Discovery omissions do not mean truncated task delivery or global absence.".into(),
            format!(
                "Complete native task prepared from {} admitted originals; {} bounded ranked hits examined, {} paths authenticated. Empty selection establishes only absence of support in this bounded task; host delivery is separately accounted.",
                self.decisions.iter().filter(|d| d.outcome == Outcome::Admitted).count(),
                self.discovery_returned_hits,
                self.authenticated_paths.len(),
            ),
        ]
    }
}

pub(super) fn discover(
    reader: &QuerySnapshot,
    meter: &mut Meter,
    query: &str,
    request: &ContextRequest,
) -> Result<Admission> {
    if request.documents.limits.hits > MAX_HITS
        || request.documents.limits.candidates > MAX_CANDIDATES
    {
        return Err(usage(
            "automatic originals admit at most 10 hits and 80 candidates",
        ));
    }
    meter.check()?;
    let hits = super::super::lexical::search_context_catalog(
        reader,
        query,
        &request.documents,
        request.documents.filters.include_historical,
    )?;
    meter.check()?;
    let mut admission = Admission {
        policy: "ranked_whole_owner_greedy_no_refill_v1",
        discovery_candidate_count: hits.candidate_count,
        discovery_returned_hits: hits.hits.len(),
        discovery_omitted_candidates: hits.omitted_candidates,
        discovery_truncated: hits.truncated,
        max_hits: request.documents.limits.hits,
        max_candidates: request.documents.limits.candidates,
        metadata_authority: "pinned_catalog; only authenticated_paths receive canonical proof",
        authenticated_paths: Vec::new(),
        decisions: Vec::new(),
    };
    let mut owners = BTreeSet::new();
    let mut raw_bytes = 0usize;
    let mut indexed_bytes = 0usize;
    for (rank, hit) in hits.hits.into_iter().enumerate() {
        meter.check()?;
        let mut decision = Decision {
            rank: rank + 1,
            locator: hit.locator.clone(),
            source_id: hit.source_id.clone(),
            source_revision: hit.owner_revision.clone(),
            original_bytes: None,
            indexed_bytes: None,
            outcome: Outcome::NotCaptured,
        };
        if let (Some(source), Some(revision)) = (hit.source_id, hit.owner_revision) {
            decision.outcome = Outcome::Ineligible;
            if matches!(
                hit.eligibility,
                Eligibility::Current | Eligibility::Historical | Eligibility::Withdrawn
            ) && (request.documents.filters.include_historical
                || hit.eligibility == Eligibility::Current)
            {
                if !owners.insert((source, revision)) {
                    decision.outcome = Outcome::DuplicateOwner;
                } else {
                    let (raw, indexed) = reader
                        .original_document_sizes(&hit.locator.path)?
                        .ok_or_else(|| {
                            stale("discovered original size is absent from pinned catalog")
                        })?;
                    decision.original_bytes = Some(raw);
                    decision.indexed_bytes = Some(indexed);
                    if raw == 0 {
                        decision.outcome = Outcome::EmptyOriginal;
                    } else if raw > packet::MAX_ORIGINAL_BYTES.saturating_sub(raw_bytes) {
                        decision.outcome = Outcome::OriginalBytes;
                    } else if indexed
                        > packet::MAX_ORIGINAL_INDEXED_BYTES.saturating_sub(indexed_bytes)
                    {
                        decision.outcome = Outcome::IndexedBytes;
                    } else {
                        raw_bytes += raw;
                        indexed_bytes += indexed;
                        admission.authenticated_paths.push(hit.locator.path);
                        // The same combined proof also covers originals later omitted
                        // for JSON/line-coordinate overhead; that work stays disclosed.
                        decision.outcome = Outcome::SerializedInputBytes;
                    }
                }
            }
        }
        admission.decisions.push(decision);
    }
    reader.check_query_budget()?;
    meter.check()?;
    Ok(admission)
}

#[derive(Serialize)]
struct AutoPayload<'a> {
    version: OriginalSelectionVersion,
    authority_domain: &'static str,
    binding: Binding<'a>,
    policy: Policy,
    admission: &'a Admission,
    task_delivery: &'static str,
    originals: &'a [SelectionOriginal],
}

#[derive(Serialize)]
struct AutoTask<'a> {
    instructions: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    packet_fingerprint: Option<&'a Blake3Hash>,
    payload: &'a AutoPayload<'a>,
}

fn build_packet(
    query: &str,
    request: &ContextRequest,
    originals_request: &OriginalSelectionRequest,
    reader: &dyn QueryCatalog,
    originals: Vec<SelectionOriginal>,
    admission: &Admission,
) -> Result<Option<SelectionPacket>> {
    reader.check_query_budget()?;
    let dependency = reader.dependency_fingerprint()?;
    let payload = AutoPayload {
        version: OriginalSelectionVersion::V2,
        authority_domain: "lwiki.context-original-selection.authority.v2",
        binding: Binding {
            query,
            request,
            originals_request,
            snapshot: reader.snapshot(),
            publication_id: reader.publication_id(),
            dependency_fingerprint: &dependency,
        },
        policy: Policy {
            max_paths: MAX_HITS,
            max_original_bytes: packet::MAX_ORIGINAL_BYTES,
            max_indexed_bytes: packet::MAX_ORIGINAL_INDEXED_BYTES,
            max_input_bytes: originals_request.max_input_bytes,
            max_application_input_bytes: packet::MAX_ORIGINAL_INPUT_BYTES + TRANSPORT_BYTES,
            transport_reserved_bytes: TRANSPORT_BYTES,
            max_reply_bytes: REPLY_BYTES,
            max_ranges: RANGE_COUNT,
            max_ranges_per_owner: OWNER_RANGES,
            max_range_bytes: request.documents.limits.excerpt_bytes,
            token_accounting: "estimated_utf8_bytes_div4_ceil",
            selected_evidence: "exact_original_utf8_ranges_only",
        },
        admission,
        task_delivery: "complete_utf8_no_original_truncation",
        originals: &originals,
    };
    let unsigned = AutoTask {
        instructions: INSTRUCTIONS,
        packet_fingerprint: None,
        payload: &payload,
    };
    if packet::encoded_size_with_limit(&unsigned, originals_request.max_input_bytes)?
        > originals_request.max_input_bytes
    {
        reader.check_query_budget()?;
        return Ok(None);
    }
    let fingerprint = Blake3Hash::digest(canonical_json(&unsigned)?);
    let signed = AutoTask {
        packet_fingerprint: Some(&fingerprint),
        ..unsigned
    };
    if packet::encoded_size_with_limit(&signed, originals_request.max_input_bytes)?
        > originals_request.max_input_bytes
    {
        reader.check_query_budget()?;
        return Ok(None);
    }
    let selector_input = String::from_utf8(canonical_json(&signed)?)
        .map_err(|_| WikiError::invalid("automatic original task is not UTF-8"))?;
    let input_bytes = selector_input.len();
    reader.check_query_budget()?;
    Ok(Some(SelectionPacket {
        fingerprint,
        selector_input,
        candidate_count: admission.discovery_candidate_count,
        input_bytes,
        estimated_tokens: input_bytes.div_ceil(4),
        omitted_candidates: admission
            .decisions
            .iter()
            .filter(|d| d.outcome != Outcome::Admitted)
            .count()
            + admission
                .discovery_candidate_count
                .saturating_sub(admission.discovery_returned_hits)
                .max(admission.discovery_omitted_candidates),
        cards: Vec::new(),
        originals,
    }))
}

pub(super) fn admit_packet(
    query: &str,
    request: &ContextRequest,
    originals_request: &OriginalSelectionRequest,
    reader: &dyn QueryCatalog,
    originals: Vec<SelectionOriginal>,
    admission: &mut Admission,
    meter: &mut Meter,
) -> Result<SelectionPacket> {
    // Establish whether the fixed metadata/policy itself fits. Remaining owners
    // already carry the longer omission spelling, so later failed admission
    // cannot invalidate a previously fitting packet through status growth.
    meter.check()?;
    let mut packet = build_packet(
        query,
        request,
        originals_request,
        reader,
        Vec::new(),
        admission,
    )?
    .ok_or_else(|| budget("automatic original policy/omissions exceed task byte limit"))?;
    for original in originals {
        meter.check()?;
        let decision = admission
            .decisions
            .iter_mut()
            .find(|decision| decision.locator.path == original.locator.path)
            .ok_or_else(|| stale("authenticated original is outside discovery admission"))?;
        if decision.locator != original.locator
            || decision.source_id.as_ref() != Some(&original.source_id)
            || decision.source_revision.as_ref() != Some(&original.source_revision)
        {
            return Err(stale(
                "discovery identity differs from authenticated original",
            ));
        }
        decision.outcome = Outcome::Admitted;
        let mut proposed = packet.originals.clone();
        proposed.push(original.clone());
        match build_packet(
            query,
            request,
            originals_request,
            reader,
            proposed,
            admission,
        )? {
            Some(admitted) => packet = admitted,
            None => {
                admission
                    .decisions
                    .iter_mut()
                    .find(|decision| decision.locator.path == original.locator.path)
                    .expect("admission decision retained")
                    .outcome = Outcome::SerializedInputBytes;
            }
        }
    }
    meter.check()?;
    Ok(packet)
}

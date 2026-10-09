//! Exposed DEVELOPMENT Gate A diagnostic; never a publication capability or CLI pass.
use super::{
    Catalog,
    file_types::{BuildIdentity, CatalogSelection},
    normalized_build::{BuildLimits, NormalizedBuilder},
    policy_facts::{NormalizedPolicyFacts, PolicyKind, PolicyRow, PolicyState},
    policy_projection::{PolicyInputAccess, project_graph_policy_probe},
    query_types::QueryReadLimits,
    scan, selector,
};
use crate::{
    changes::{ChangeDraft, ChangeEngine, OriginPolicy},
    domain::*,
    graph::{
        decision_types::ENTITY_DECISIONS_SCHEMA,
        decisions::{build_draft_for_remap_probe, validate_entity_decisions},
        extraction_types::*,
        import::{load_extraction, stage_import},
        normalized_input::{GraphInputRequest, capture_graph_inputs},
        normalized_meter::{GRAPH_PROCESSING_BYTES, GraphOperationMeter},
        packet::{build_packet, load_packet},
        policy_inputs::{
            PolicyInputKey, PolicyWork, policy_membership_keys, remap_probe_membership_bytes,
        },
        remap_input_probe::{RemapInputProbeMode, with_mode},
        resolution::{build_draft_for_selected_probe, validate_resolution},
        resolution_types::*,
        review::{build_draft_v2, validate_review},
        review_types::*,
        wire::validate_response,
    },
    records::{ParsedNote, parse_note},
    sources::{
        CaptureRequest, ExtractionInput, SourceOrigin, SourceStore, SourceView,
        revision::{common, record_bytes},
    },
    vault::{ExpectedState, VaultFs, VaultRoot, WriterPermit},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    time::Duration,
};

fn id(s: &str) -> RecordId {
    RecordId::new(s).unwrap()
}
fn path(s: impl Into<String>) -> VaultRelativePath {
    VaultRelativePath::new(s).unwrap()
}
fn note(kind: RecordKind, name: &RecordId, extra: Value) -> Vec<u8> {
    let mut fields = common(name, kind, name.as_str());
    fields.extend(
        extra
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone())),
    );
    record_bytes(
        CanonicalRecord::new(fields).unwrap(),
        b"Owned development fixture.\n",
    )
    .unwrap()
}

struct Fixture {
    _temp: tempfile::TempDir,
    fs: VaultFs,
    writer: WriterPermit,
    catalog: Catalog,
    generation: u64,
    owned: BTreeMap<VaultRelativePath, Vec<u8>>,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let vault = id("vault_graph_dev");
        fs::write(
            temp.path().join("WIKI.md"),
            note(RecordKind::Vault, &vault, json!({})),
        )
        .unwrap();
        let handle = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let writer = WriterPermit::acquire(handle.root(), Duration::ZERO).unwrap();
        let mut f = Self {
            _temp: temp,
            catalog: Catalog::new(handle.clone(), vault),
            fs: handle,
            writer,
            generation: 0,
            owned: BTreeMap::new(),
        };
        f.owned.insert(
            path("WIKI.md"),
            fs::read(f.fs.root().path().join("WIKI.md")).unwrap(),
        );
        for entity in ["entity_dev_ada", "entity_dev_acme"] {
            f.put(
                path(format!("knowledge/entities/{entity}.md")),
                note(
                    RecordKind::Entity,
                    &id(entity),
                    json!({"wiki_status":"active", "wiki_entity_type":
                    if entity.ends_with("ada") {"person"} else {"organization"}}),
                ),
            );
        }
        f
    }
    fn put(&mut self, target: VaultRelativePath, bytes: Vec<u8>) {
        let disk = self.fs.root().path().join(target.as_str());
        fs::create_dir_all(disk.parent().unwrap()).unwrap();
        fs::write(disk, &bytes).unwrap();
        self.owned.insert(target, bytes);
    }
    // Fixture installation is intentionally distinct from command publication.
    fn install(&mut self, draft: ChangeDraft) {
        for op in draft.operations {
            let actual = self
                .owned
                .get(&op.target)
                .map_or(ExpectedState::Absent, |bytes| {
                    ExpectedState::Hash(Blake3Hash::digest(bytes))
                });
            assert_eq!(actual, op.expected, "fixture before-image {}", op.target);
            self.put(op.target, op.proposed.expect("fixture never deletes"));
        }
    }
    fn reconstruct(&mut self) -> Result<NormalizedPolicyFacts> {
        self.generation += 1;
        let identity = BuildIdentity {
            selection: CatalogSelection::new(id("vault_graph_dev"), self.generation)?,
            origin: None,
            vector_cache_lost: false,
            vector_loss_unknown: false,
        };
        selector::prepare(&self.fs, &self.writer, &identity.selection)?;
        let mut builder =
            NormalizedBuilder::begin(&self.fs, &self.writer, identity, BuildLimits::default())?;
        let input = scan::scan_input(&self.fs, &id("vault_graph_dev"))?;
        let projected = scan::project_normalized_with_sink(&self.fs, &input, false, &mut builder)?;
        let policy = projected
            .facts
            .policy
            .clone()
            .expect("real complete policy reconstruction");
        let complete = builder.finish_normalized(&projected)?;
        selector::publish(
            &self.fs,
            &self.writer,
            &complete.identity.selection,
            Duration::ZERO,
        )?;
        Ok(policy)
    }
    fn view(&self) -> Result<SourceView<'_>> {
        // Allowed only during complete, disposable fixture construction.
        SourceView::from_fs_bounded(&self.fs, 64 * 1024 * 1024, 4096)
    }
    fn unchanged(&self) {
        for (p, bytes) in &self.owned {
            assert_eq!(
                fs::read(self.fs.root().path().join(p.as_str())).unwrap(),
                *bytes,
                "diagnostic changed owned canonical/asset {}",
                p
            );
        }
    }
    fn import(
        &mut self,
        source: &RecordId,
        revision: &RecordId,
        windows: &[ByteSpan],
        quotes: &[Vec<(ByteSpan, String)>],
    ) -> Result<RecordId> {
        let export = ExportRequest {
            source_id: source.clone(),
            revision_id: Some(revision.clone()),
            windows: windows.to_vec(),
            limits: ExtractionLimits::default(),
            candidate_context: vec![],
        };
        let packet = build_packet(&self.view()?, &export)?;
        self.install(packet.draft.expect("fresh packet"));
        let view = self.view()?;
        let loaded = load_packet(&view, &packet.packet.packet_id)?;
        let assertions: Vec<_> = quotes
            .iter()
            .enumerate()
            .map(|(a, group)| {
                json!({
                    "id":format!("a{a}"),"subject":"m_ada","predicate":"works_for",
                    "object":{"kind":"mention","mention_id":"m_acme"},
                    "negated":false,"modality":"asserted",
                    "evidence":group.iter().map(|(span,quote)|json!({"window_id":format!("w{}",a+1),
                        "stance":"supports","quote":quote,"span":span})).collect::<Vec<_>>()
                })
            })
            .collect();
        let raw = serde_json::to_vec(&json!({"schema":EXTRACTION_SCHEMA,
            "packet_id":packet.packet.packet_id,"packet_fingerprint":packet.packet.packet_fingerprint,
            "mentions":[{"id":"m_ada","window_id":"w1","label":"Ada","type":"person",
                "quote":"Ada","span":{"start":0,"end":3}},
                {"id":"m_acme","window_id":"w1","label":"Acme","type":"organization",
                "quote":"Acme","span":{"start":14,"end":18}}],
            "assertions":assertions,"unresolved":[]}))
        .unwrap();
        let validated = validate_response(&loaded, &view, &raw)?;
        let engine = ChangeEngine::new(self.fs.clone())?;
        let imported = stage_import(
            &engine,
            &self.writer,
            &validated,
            OriginPolicy::ReuseOrConflict,
        )?;
        let prepared = imported.prepared.expect("new fixture import");
        let retained = engine.inspect_history(&prepared.change_id)?;
        // Install exact constructor payloads; no legacy executor or partial validator.
        let mut payloads = vec![];
        for (i, op) in retained.manifest.operations.iter().enumerate() {
            let bytes = engine
                .verify_payload(
                    &prepared.change_id,
                    i,
                    "proposed",
                    &op.target,
                    &op.after,
                    &op.after_payload,
                )?
                .expect("import payload");
            payloads.push((op.target.clone(), bytes));
        }
        drop(view);
        for (p, b) in payloads {
            self.put(p, b);
        }
        Ok(imported
            .extraction
            .record
            .expect("Extraction locator")
            .record_id)
    }
    fn resolve_request(&self, extraction: &RecordId) -> Result<ResolutionRequest> {
        let view = self.view()?;
        let artifact = load_extraction(&view, extraction)?;
        let mappings = [("m_ada", "entity_dev_ada"), ("m_acme", "entity_dev_acme")]
            .into_iter()
            .map(|(mention, entity)| {
                let (_, note) = view.resolve(&id(entity), RecordKind::Entity, None)?;
                Ok(ResolutionMapping::BindMention {
                    mention_id: PacketLocalId::new(mention)?,
                    entity_id: id(entity),
                    expected_entity_hash: note.source_hash.clone(),
                    reason: "Explicit development identity binding".into(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(ResolutionRequest {
            schema: RESOLUTION_SCHEMA.into(),
            extraction_id: extraction.clone(),
            expected_hash: artifact.locator().observed_hash.clone(),
            mappings,
        })
    }
}

struct Access<'a> {
    fs: &'a VaultFs,
    meter: &'a mut GraphOperationMeter,
    loaded: BTreeMap<VaultRelativePath, Blake3Hash>,
    steps: usize,
}
impl PolicyInputAccess for Access<'_> {
    fn note(&mut self, p: &VaultRelativePath, expected: &Blake3Hash) -> Result<ParsedNote> {
        let io_error = |e: std::io::Error| WikiError::new(ErrorCode::Internal, e.to_string());
        let limit = self.meter.remaining_file_bytes()?;
        self.meter.probe()?;
        let file = fs::File::open(self.fs.root().resolve(p)?).map_err(io_error)?;
        if file.metadata().map_err(io_error)?.len() > limit as u64 {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "policy witness exceeds remaining bytes",
            ));
        }
        let mut bytes = Vec::new();
        file.take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(io_error)?;
        if bytes.len() > limit {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "policy witness grew",
            ));
        }
        self.meter.capture_read(p, bytes.len())?;
        // Hash, parse and owned parsed-note copy are processing work.
        for _ in 0..3 {
            self.meter.charge_processing(bytes.len())?;
        }
        let note = parse_note(&bytes);
        if note.source_hash != *expected {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "policy probe witness changed",
            ));
        }
        self.loaded.insert(p.clone(), expected.clone());
        Ok(note)
    }
    fn work(&mut self, work: PolicyWork) -> Result<()> {
        self.steps = self
            .steps
            .checked_add(work.steps)
            .ok_or_else(|| WikiError::invalid("work overflow"))?;
        self.meter.charge_processing(work.bytes)
    }
}

fn capture_request(text: Vec<u8>) -> CaptureRequest {
    CaptureRequest {
        title: "Owned DEV16/256 source".into(),
        origin_kind: SourceOrigin::LocalFile,
        origin: "owned-development.md".into(),
        original: text,
        extraction: ExtractionInput::Utf8Preserve,
        media_type: Some("text/markdown".into()),
    }
}
fn diagnostic(report: &mut Value) -> Result<()> {
    let mut text = String::new();
    let mut windows = vec![];
    let mut quotes = vec![];
    for a in 0..16 {
        let group_start = text.len();
        let mut group = vec![];
        for e in 0..16 {
            let quote = format!(
                "Ada works for Acme on project DEV{a:02}, observation {e:02}; this development record confirms the employment relationship.\n"
            );
            let start = text.len();
            text.push_str(&quote);
            group.push((ByteSpan::new(start as u64, text.len() as u64)?, quote));
        }
        windows.push(ByteSpan::new(group_start as u64, text.len() as u64)?);
        quotes.push(group);
    }
    let mut f = Fixture::new();
    report["stage"] = json!("fixture source capture/import");
    let capture =
        SourceStore::new(f.fs.clone()).plan_capture(capture_request(text.clone().into_bytes()))?;
    let source = capture.source_id;
    let revision = capture.revision_id;
    f.install(capture.draft.expect("fresh capture"));
    let first = f.import(&source, &revision, &windows, &quotes)?;
    f.reconstruct()?;
    report["stage"] = json!("first selected resolution");
    let request = f.resolve_request(&first)?;
    let reader = f.catalog.query_snapshot(QueryReadLimits::default())?;
    let mut meter = GraphOperationMeter::new();
    let selected = capture_graph_inputs(
        &f.fs,
        &reader,
        GraphInputRequest::Resolve(&request),
        &mut meter,
    )?;
    let view = selected.view_metered(&f.fs, &mut meter)?;
    let validated = validate_resolution(&view, &serde_json::to_vec(&request).unwrap())?;
    let draft = build_draft_for_selected_probe(&view, &validated)?;
    assert_eq!(draft.operations.len(), 275);
    drop(view);
    drop(reader);
    f.install(draft);
    f.reconstruct()?;

    report["stage"] = json!("first complete selected v2 review");
    let view = f.view()?;
    let artifact = load_extraction(&view, &first)?.artifact().clone();
    let decisions = artifact
        .allocations
        .assertions
        .iter()
        .map(|(local, assertion)| {
            let (_, note) = view.resolve(assertion, RecordKind::Assertion, None)?;
            let evidence_checks = artifact.allocations.evidence[local]
                .iter()
                .map(|evidence| {
                    let (_, note) = view.resolve(evidence, RecordKind::Evidence, None)?;
                    Ok(EvidenceCheck {
                        evidence_id: evidence.clone(),
                        expected_hash: note.source_hash.clone(),
                        assessment: EvidenceAssessment::Supports,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(AssertionReview {
                assertion_id: assertion.clone(),
                expected_hash: note.source_hash.clone(),
                decision: ReviewDecision::Accept,
                reason: "Complete exact source evidence checked".into(),
                evidence_checks,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let review = ReviewRequest {
        schema: GRAPH_REVIEW_SCHEMA.into(),
        decisions,
        supersedes: vec![],
    };
    drop(view);
    let reader = f.catalog.query_snapshot(QueryReadLimits::default())?;
    let mut meter = GraphOperationMeter::new();
    let capture = capture_graph_inputs(
        &f.fs,
        &reader,
        GraphInputRequest::Review(&review),
        &mut meter,
    );
    report["first_review_capture_query"] = json!({"rows":reader.usage().rows,"bytes":reader.usage().bytes,
        "usage":meter.usage()});
    if capture.is_err() {
        f.unchanged();
        report["owned_canonical_unchanged"] = json!(true);
    }
    let selected = capture?;
    let view = selected.view_metered(&f.fs, &mut meter)?;
    let validated = validate_review(&view, &serde_json::to_vec(&review).unwrap())?;
    let (draft, carrier) = build_draft_v2(&view, &validated)?;
    assert_eq!(carrier.receipt.evidence_proofs.len(), 256);
    assert_eq!(carrier.receipt.assertion_proofs.len(), 16);
    assert!(carrier.receipt.successor_templates.is_empty());
    assert_eq!(draft.operations.len(), 32);
    let family: BTreeMap<_, _> = carrier
        .receipt
        .allocations
        .decisions
        .values()
        .map(|i| {
            let p = carrier.receipt.record_paths[i].clone();
            let b = draft
                .operations
                .iter()
                .find(|op| op.target == p)
                .unwrap()
                .proposed
                .as_ref()
                .unwrap();
            (p, b.len())
        })
        .collect();
    report["first_review"] = json!({"carrier_path":carrier.carrier_path,"carrier_bytes":family[&carrier.carrier_path],
        "canonical_family":family,"operations":draft.operations.len(),"query_rows":reader.usage().rows,
        "query_bytes":reader.usage().bytes,"capture_usage":meter.usage()});
    drop(view);
    drop(reader);
    f.install(draft);
    let old_policy = f.reconstruct()?;
    assert_eq!(
        old_policy.states[&PolicyKind::Review],
        PolicyState::Verified
    );
    report["published_policy"] = json!({"states":old_policy.states,"traces":old_policy.traces});

    report["stage"] = json!("refresh source and import fresh extraction");
    text.push_str("Refresh marker: a new immutable source revision.\n");
    let refreshed =
        SourceStore::new(f.fs.clone()).plan_refresh(&source, capture_request(text.into_bytes()))?;
    let second_revision = refreshed.revision_id;
    f.install(refreshed.draft.expect("changed source"));
    let second = f.import(&source, &second_revision, &windows, &quotes)?;
    f.reconstruct()?;
    let request = f.resolve_request(&second)?;
    let reader = f.catalog.query_snapshot(QueryReadLimits::default())?;
    let mut meter = GraphOperationMeter::new();
    report["stage"] = json!("second selected capture");
    let capture = capture_graph_inputs(
        &f.fs,
        &reader,
        GraphInputRequest::Resolve(&request),
        &mut meter,
    );
    report["second_capture_query"] =
        json!({"rows":reader.usage().rows,"bytes":reader.usage().bytes});
    let query_bytes_captured = reader.usage().bytes;
    if capture.is_err() {
        f.unchanged();
        report["owned_canonical_unchanged"] = json!(true);
    }
    let selected = capture?;
    let view = selected.view_metered(&f.fs, &mut meter)?;
    let validated = validate_resolution(&view, &serde_json::to_vec(&request).unwrap())?;
    report["stage"] = json!("second semantic constructor");
    let draft = build_draft_for_selected_probe(&view, &validated)?;
    assert_eq!(draft.operations.len(), 275);
    let targets: BTreeSet<_> = draft.operations.iter().map(|o| o.target.clone()).collect();
    let mut before = BTreeMap::new();
    let mut overlay = BTreeMap::new();
    for op in &draft.operations {
        if let Some(bytes) = f.owned.get(&op.target) {
            before.insert(op.target.clone(), parse_note(bytes));
        }
        let bytes = op.proposed.as_ref().expect("full materialization");
        meter.charge_processing(bytes.len())?;
        overlay.insert(op.target.clone(), parse_note(bytes));
    }
    let changed = before
        .iter()
        .chain(&overlay)
        .try_fold(BTreeSet::new(), |mut keys, (p, n)| {
            meter.charge_processing(remap_probe_membership_bytes(n)?)?;
            keys.extend(policy_membership_keys(p, n)?);
            Ok::<_, WikiError>(keys)
        })?;
    let mut operation_counts = BTreeMap::new();
    for note in overlay.values() {
        let kind = note
            .canonical
            .as_ref()
            .expect("semantic canonical owner")
            .kind()
            .as_str();
        *operation_counts.entry(kind).or_insert(0usize) += 1;
    }
    assert_eq!(
        operation_counts,
        BTreeMap::from([
            ("assertion", 16),
            ("decision", 2),
            ("evidence", 256),
            ("extraction", 1)
        ])
    );
    let affected = reader.policy_affected(&changed, &targets)?;
    report["second_proposal"] = json!({"writes":draft.operations.len(),"operation_counts":operation_counts,"owners":targets,
        "bytes":draft.operations.iter().map(|o|o.proposed.as_ref().unwrap().len()).sum::<usize>(),
        "changed_membership_keys":changed,"affected_policy":affected});
    assert!(changed.contains(&PolicyInputKey::RemapReceiptCandidates));
    assert!(affected.contains(&PolicyKind::Review));
    report["stage"] = json!("real prospective policy projection");
    let query_bytes_before = query_bytes_captured;
    let mut access = Access {
        fs: &f.fs,
        meter: &mut meter,
        loaded: BTreeMap::new(),
        steps: 0,
    };
    let projection = project_graph_policy_probe(&reader, &before, &overlay, &mut access);
    report["policy_projection"] = json!({"loaded_paths":access.loaded,"steps":access.steps,
        "query_rows":reader.usage().rows,"query_bytes":reader.usage().bytes,
        "usage":access.meter.usage()});
    let loaded = access.loaded.clone();
    drop(access);
    meter.charge_processing(reader.usage().bytes.saturating_sub(query_bytes_before))?;
    if projection.is_err() {
        f.unchanged();
        report["owned_canonical_unchanged"] = json!(true);
    }
    let delta = projection?;
    assert_eq!(
        delta
            .replacements
            .iter()
            .flat_map(|r| &r.rows)
            .filter(|r| matches!(
                r,
                PolicyRow::State {
                    kind: PolicyKind::Review,
                    ..
                }
            ))
            .count(),
        1,
        "the prospective Review state must be present exactly once"
    );
    let mut guards: BTreeMap<_, _> = selected
        .dependencies()
        .iter()
        .chain(&draft.read_preconditions)
        .filter(|d| !targets.contains(&d.path))
        .map(|d| (d.path.clone(), d.expected.clone()))
        .collect();
    for replacement in &delta.replacements {
        for row in &replacement.rows {
            if let PolicyRow::ReadPath { path, hash, .. } = row {
                if !targets.contains(path) {
                    guards.insert(path.clone(), ExpectedState::Hash(hash.clone()));
                }
            }
            if let PolicyRow::State {
                kind: PolicyKind::Review,
                state,
            } = row
            {
                assert_eq!(*state, PolicyState::Verified);
            }
        }
    }
    for (p, h) in loaded {
        if !targets.contains(&p) {
            assert_eq!(guards.get(&p), Some(&ExpectedState::Hash(h)));
        }
    }
    assert!(
        guards.contains_key(&carrier.carrier_path),
        "actual old whole carrier must be guarded"
    );
    let guard_bytes: BTreeMap<_, _> = guards
        .iter()
        .map(|(p, expected)| {
            let size = match expected {
                ExpectedState::Absent => 0,
                ExpectedState::Hash(h) => {
                    let bytes = f.owned.get(p).expect("guard is an owned fixture input");
                    assert_eq!(*h, Blake3Hash::digest(bytes));
                    bytes.len()
                }
            };
            (p.clone(), size)
        })
        .collect();
    let total = guard_bytes.values().sum::<usize>();
    let floor = total * 3 * draft.operations.len() + meter.usage().processing_bytes;
    report["stage"] = json!("pre-mutation three-pass guard floor");
    report["guard_floor"] = json!({"paths":guard_bytes,"path_count":guards.len(),"unchanged_bytes":total,
        "passes_per_write":3,"processing_already_used":meter.usage().processing_bytes,
        "required_bytes":floor,"budget_bytes":GRAPH_PROCESSING_BYTES,
        "prospective_fts_counted_rows":null,"row_measurement":"unavailable: no sealed full graph projector"});
    let admission = meter.preflight_guard_floor(total, draft.operations.len());
    if floor > GRAPH_PROCESSING_BYTES {
        assert_eq!(admission.unwrap_err().code, ErrorCode::BudgetExceeded);
        report["admission"] = json!("NO_GO: mandatory guard floor exceeds budget");
    } else {
        admission?;
        report["admission"] =
            json!("INCONCLUSIVE: lower bound fits; complete projector unavailable");
    }
    f.unchanged();
    report["owned_canonical_unchanged"] = json!(true);
    Ok(())
}

#[test]
fn dev_16_256_review_then_refreshed_bind_existing_guard_diagnostic() {
    let mut report = json!({"schema":"normalized-graph-gate-a-development.v1","workflow_pass":false,
        "layout":"normalized original layout only","processing_measurement":"instrumented selected capture, closed view, witness and policy work; semantic constructor internal copies not fully metered; floor is a lower bound","stage":"owned fixture initialization"});
    let result = diagnostic(&mut report);
    if let Err(error) = &result {
        report["error"] = json!(error);
        report["admission"] = json!("NO_GO: diagnostic stopped at first actual failure");
    }
    println!(
        "NORMALIZED_GRAPH_GATE_A {}",
        serde_json::to_string(&report).unwrap()
    );
    // Resource refusal is a measured blocker, never workflow acceptance. Other
    // errors fail the diagnostic so invalid fixture semantics cannot look positive.
    if let Err(error) = result {
        assert_eq!(
            error.code,
            ErrorCode::BudgetExceeded,
            "unexpected diagnostic failure: {error}"
        );
    }
}

// Fixed DEVELOPMENT populations. Construction is outside operation accounting;
// neither fixture installation nor these complete catalogs admit a graph Change.
struct FrozenWorkflow {
    owned: BTreeMap<VaultRelativePath, Vec<u8>>,
    request: ResolutionRequest,
    proposal: ChangeDraft,
    carrier: VaultRelativePath,
    first_review_owned: BTreeMap<VaultRelativePath, Vec<u8>>,
    source_prefix: String,
}
fn copied_fixture(owned: &BTreeMap<VaultRelativePath, Vec<u8>>) -> Fixture {
    let mut f = Fixture::new();
    for (p, bytes) in owned {
        f.put(p.clone(), bytes.clone());
    }
    assert_eq!(&f.owned, owned);
    f
}
fn hashes(owned: &BTreeMap<VaultRelativePath, Vec<u8>>) -> BTreeMap<VaultRelativePath, Blake3Hash> {
    owned
        .iter()
        .map(|(p, b)| (p.clone(), Blake3Hash::digest(b)))
        .collect()
}
fn add_remap_source(f: &mut Fixture) {
    f.put(
        path("knowledge/entities/entity_dev_merge_source.md"),
        note(
            RecordKind::Entity,
            &id("entity_dev_merge_source"),
            json!({"wiki_status":"active","wiki_entity_type":"person"}),
        ),
    );
}
fn bind_remap_source(f: &Fixture, request: &mut ResolutionRequest) -> Result<()> {
    let view = f.view()?;
    let (_, source) = view.resolve(&id("entity_dev_merge_source"), RecordKind::Entity, None)?;
    request.mappings[0] = ResolutionMapping::BindMention {
        mention_id: PacketLocalId::new("m_ada")?,
        entity_id: id("entity_dev_merge_source"),
        expected_entity_hash: source.source_hash.clone(),
        reason: "Distinct active identity before real merge".into(),
    };
    Ok(())
}
fn install_resolution(f: &mut Fixture, request: &ResolutionRequest) -> Result<()> {
    let view = f.view()?;
    let validated = validate_resolution(&view, &serde_json::to_vec(request).unwrap())?;
    let draft = build_draft_for_selected_probe(&view, &validated)?;
    drop(view);
    f.install(draft);
    Ok(())
}
fn ordinary_history(f: &mut Fixture, report: &mut Value) -> Result<()> {
    // Four declared separate sources/imports, each with two genuine ordinary binds.
    for h in 0..4 {
        report["stage"] = json!(format!("ordinary history import/resolution {h}"));
        let quote = format!("Ada works for Acme on separate owned history {h}.\n");
        let span = ByteSpan::new(0, quote.len() as u64)?;
        let mut request = capture_request(quote.clone().into_bytes());
        request.origin = format!("owned-history-{h}.md");
        let capture = SourceStore::new(f.fs.clone()).plan_capture(request)?;
        let source = capture.source_id;
        let revision = capture.revision_id;
        f.install(capture.draft.expect("fresh history source"));
        let extraction = f.import(&source, &revision, &[span], &[vec![(span, quote)]])?;
        let request = f.resolve_request(&extraction)?;
        install_resolution(f, &request)?;
    }
    f.reconstruct()?;
    report["ordinary_history"] = json!({"imports":4,"binds_per_import":2});
    Ok(())
}
fn entity_request(f: &Fixture, split: bool) -> Result<Value> {
    let source = id("entity_dev_merge_source");
    let target = json!({"kind":"ExistingEntity","entity_id":"entity_dev_ada"});
    let target = if split {
        json!({"kind":"NewEntity","key":"left"})
    } else {
        target
    };
    let view = f.view()?;
    let mut expected = vec![];
    let mut remaps = vec![];
    for (p, bytes) in &f.owned {
        let n = parse_note(bytes);
        let Some(r) = &n.canonical else { continue };
        expected.push(json!({"record_id":r.id(),"hash":n.source_hash}));
        if r.kind() == RecordKind::Assertion {
            for field in ["subject_id", "object_id"] {
                if r.string(&format!("wiki_{field}")) == Some(source.as_str()) {
                    remaps.push(
                        json!({"kind":"Assertion","assertion_id":r.id(),"field":field,
                        "old_entity_id":source,"target":target}),
                    );
                }
            }
        }
        if r.kind() == RecordKind::Extraction {
            let artifact = load_extraction(&view, r.id())?.artifact().clone();
            for (mention, binding) in artifact.bindings {
                if matches!(binding, MentionBinding::Resolved {entity_id,..} if entity_id == source)
                {
                    remaps.push(
                        json!({"kind":"Mention","extraction_id":r.id(),"mention_id":mention,
                        "old_entity_id":source,"target":target}),
                    );
                }
            }
        }
        assert_eq!(n.source_hash, Blake3Hash::digest(&f.owned[p]));
    }
    assert!(remaps.iter().any(|r| r["kind"] == "Assertion"));
    assert!(remaps.iter().any(|r| r["kind"] == "Mention"));
    let operation = if split {
        json!({"operation":"SplitEntity","source_id":source,
            "new_entities":[{"key":"left","title":"Split left","entity_type":"person"},
                {"key":"right","title":"Split right","entity_type":"person"}],
            "reason":"Exhaustive real development partition","expected_records":expected,"remaps":remaps})
    } else {
        json!({"operation":"MergeEntities","source_ids":[source],"target_id":"entity_dev_ada",
            "reason":"Exhaustive active development identity merge","expected_records":expected,"remaps":remaps})
    };
    Ok(json!({"schema":ENTITY_DECISIONS_SCHEMA,"decisions":[operation]}))
}
fn install_entity_request(f: &mut Fixture, request: &Value) -> Result<()> {
    let view = f.view()?;
    let validated = validate_entity_decisions(&view, &serde_json::to_vec(request).unwrap())?;
    let draft = build_draft_for_remap_probe(&view, &validated)?;
    drop(view);
    f.install(draft);
    Ok(())
}
fn freeze_workflow(population: &str, report: &mut Value) -> Result<FrozenWorkflow> {
    let mut text = String::new();
    let mut windows = vec![];
    let mut quotes = vec![];
    for a in 0..16 {
        let group_start = text.len();
        let mut group = vec![];
        for e in 0..16 {
            let quote = format!(
                "Ada works for Acme on project DEV{a:02}, observation {e:02}; this development record confirms the employment relationship.\n"
            );
            let start = text.len();
            text.push_str(&quote);
            group.push((ByteSpan::new(start as u64, text.len() as u64)?, quote));
        }
        windows.push(ByteSpan::new(group_start as u64, text.len() as u64)?);
        quotes.push(group);
    }
    let mut f = Fixture::new();
    if population != "A" {
        ordinary_history(&mut f, report)?;
    }
    if population == "C" {
        add_remap_source(&mut f);
    }
    report["stage"] = json!("fixture source capture/import");
    let capture =
        SourceStore::new(f.fs.clone()).plan_capture(capture_request(text.clone().into_bytes()))?;
    let source = capture.source_id;
    let revision = capture.revision_id;
    f.install(capture.draft.expect("fresh capture"));
    let first = f.import(&source, &revision, &windows, &quotes)?;
    f.reconstruct()?;
    report["stage"] = json!("first selected resolution");
    let mut request = f.resolve_request(&first)?;
    if population == "C" {
        bind_remap_source(&f, &mut request)?;
    }
    let reader = f.catalog.query_snapshot(QueryReadLimits::default())?;
    let mut meter = GraphOperationMeter::new();
    let selected = capture_graph_inputs(
        &f.fs,
        &reader,
        GraphInputRequest::Resolve(&request),
        &mut meter,
    )?;
    let view = selected.view_metered(&f.fs, &mut meter)?;
    let validated = validate_resolution(&view, &serde_json::to_vec(&request).unwrap())?;
    let draft = build_draft_for_selected_probe(&view, &validated)?;
    assert_eq!(draft.operations.len(), 275);
    drop(view);
    drop(reader);
    f.install(draft);
    f.reconstruct()?;

    if population == "C" {
        report["stage"] = json!("genuine active merge before first review");
        let merge = entity_request(&f, false)?;
        report["merge_request"] = merge.clone();
        install_entity_request(&mut f, &merge)?;
        let policy = f.reconstruct()?;
        if policy.states[&PolicyKind::Remap] != PolicyState::Verified {
            report["active_merge_policy"] = json!(policy);
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "genuine merge complete policy is not Verified",
            ));
        }
        assert!(
            policy.aliases.is_empty(),
            "a pure merge creates no AddAlias authority"
        );
        assert!(!policy.families.is_empty());
        report["active_merge_policy"] = json!(policy);
    }
    report["stage"] = json!("first complete selected v2 review");
    let view = f.view()?;
    let artifact = load_extraction(&view, &first)?.artifact().clone();
    let decisions = artifact
        .allocations
        .assertions
        .iter()
        .map(|(local, assertion)| {
            let (_, note) = view.resolve(assertion, RecordKind::Assertion, None)?;
            let evidence_checks = artifact.allocations.evidence[local]
                .iter()
                .map(|evidence| {
                    let (_, note) = view.resolve(evidence, RecordKind::Evidence, None)?;
                    Ok(EvidenceCheck {
                        evidence_id: evidence.clone(),
                        expected_hash: note.source_hash.clone(),
                        assessment: EvidenceAssessment::Supports,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(AssertionReview {
                assertion_id: assertion.clone(),
                expected_hash: note.source_hash.clone(),
                decision: ReviewDecision::Accept,
                reason: "Complete exact source evidence checked".into(),
                evidence_checks,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let review = ReviewRequest {
        schema: GRAPH_REVIEW_SCHEMA.into(),
        decisions,
        supersedes: vec![],
    };
    drop(view);
    let reader = f.catalog.query_snapshot(QueryReadLimits::default())?;
    let mut meter = GraphOperationMeter::new();
    let capture = capture_graph_inputs(
        &f.fs,
        &reader,
        GraphInputRequest::Review(&review),
        &mut meter,
    );
    report["first_review_capture_query"] = json!({"rows":reader.usage().rows,"bytes":reader.usage().bytes,
        "usage":meter.usage()});
    if capture.is_err() {
        f.unchanged();
        report["owned_canonical_unchanged"] = json!(true);
    }
    let selected = capture?;
    let view = selected.view_metered(&f.fs, &mut meter)?;
    let validated = validate_review(&view, &serde_json::to_vec(&review).unwrap())?;
    let (draft, carrier) = build_draft_v2(&view, &validated)?;
    assert_eq!(carrier.receipt.evidence_proofs.len(), 256);
    assert_eq!(carrier.receipt.assertion_proofs.len(), 16);
    assert!(carrier.receipt.successor_templates.is_empty());
    assert_eq!(draft.operations.len(), 32);
    let family: BTreeMap<_, _> = carrier
        .receipt
        .allocations
        .decisions
        .values()
        .map(|i| {
            let p = carrier.receipt.record_paths[i].clone();
            let b = draft
                .operations
                .iter()
                .find(|op| op.target == p)
                .unwrap()
                .proposed
                .as_ref()
                .unwrap();
            (p, b.len())
        })
        .collect();
    report["first_review"] = json!({"carrier_path":carrier.carrier_path,"carrier_bytes":family[&carrier.carrier_path],
        "canonical_family":family,"operations":draft.operations.len(),"query_rows":reader.usage().rows,
        "query_bytes":reader.usage().bytes,"capture_usage":meter.usage()});
    drop(view);
    drop(reader);
    f.install(draft);
    let old_policy = f.reconstruct()?;
    if old_policy.states[&PolicyKind::Review] != PolicyState::Verified {
        report["published_policy"] = json!(old_policy);
        return Err(WikiError::new(
            ErrorCode::ContentConflict,
            "first full Review complete policy is not Verified",
        ));
    }
    let first_review_owned = f.owned.clone();
    report["published_policy"] = json!({"states":old_policy.states,"traces":old_policy.traces});

    report["stage"] = json!("refresh source and import fresh extraction");
    text.push_str("Refresh marker: a new immutable source revision.\n");
    let refreshed =
        SourceStore::new(f.fs.clone()).plan_refresh(&source, capture_request(text.into_bytes()))?;
    let second_revision = refreshed.revision_id;
    f.install(refreshed.draft.expect("changed source"));
    let second = f.import(&source, &second_revision, &windows, &quotes)?;
    f.reconstruct()?;
    let request = f.resolve_request(&second)?;
    let reader = f.catalog.query_snapshot(QueryReadLimits::default())?;
    let mut meter = GraphOperationMeter::new();
    report["stage"] = json!("second selected capture");
    let capture = capture_graph_inputs(
        &f.fs,
        &reader,
        GraphInputRequest::Resolve(&request),
        &mut meter,
    );
    report["second_capture_query"] =
        json!({"rows":reader.usage().rows,"bytes":reader.usage().bytes});
    let query_bytes_captured = reader.usage().bytes;
    if capture.is_err() {
        f.unchanged();
        report["owned_canonical_unchanged"] = json!(true);
    }
    let selected = capture?;
    let view = selected.view_metered(&f.fs, &mut meter)?;
    let validated = validate_resolution(&view, &serde_json::to_vec(&request).unwrap())?;
    report["stage"] = json!("second semantic constructor");
    let draft = build_draft_for_selected_probe(&view, &validated)?;
    assert_eq!(draft.operations.len(), 275);
    drop(view);
    drop(reader);
    f.unchanged();
    Ok(FrozenWorkflow {
        owned: f.owned.clone(),
        request,
        proposal: draft,
        carrier: carrier.carrier_path,
        first_review_owned,
        source_prefix: format!("sources/{source}/"),
    })
}

fn semantic(policy: &NormalizedPolicyFacts) -> Value {
    json!({"states":policy.states,"aliases":policy.aliases,"families":policy.families,"edges":policy.edges})
}
fn replacement_output(rows: &[PolicyRow]) -> BTreeSet<String> {
    rows.iter()
        .filter(|r| {
            matches!(
                r,
                PolicyRow::State { .. }
                    | PolicyRow::Alias(_)
                    | PolicyRow::FamilyMember { .. }
                    | PolicyRow::Edge { .. }
            )
        })
        .map(|r| serde_json::to_string(r).unwrap())
        .collect()
}
fn complete_output(policy: &NormalizedPolicyFacts, kind: PolicyKind) -> Result<BTreeSet<String>> {
    let mut rows = vec![];
    policy.visit(&mut |row| {
        if matches!(&row, PolicyRow::State {kind:k,..}|PolicyRow::Edge {kind:k,..} if *k == kind)
            || (kind == PolicyKind::Remap
                && matches!(&row, PolicyRow::Alias(_) | PolicyRow::FamilyMember { .. }))
        {
            rows.push(row);
        }
        Ok(())
    })?;
    Ok(replacement_output(&rows))
}
fn paired_arm(
    frozen: &FrozenWorkflow,
    mode: RemapInputProbeMode,
    population: &str,
    report: &mut Value,
) -> Result<()> {
    // All readers, facts, certificates and meters live and die in this one mode scope.
    let mut f = copied_fixture(&frozen.owned);
    let result = (|| {
        report["stage"] = json!("complete first Review independent publication");
        let mut first_fixture = copied_fixture(&frozen.first_review_owned);
        let first_review = first_fixture.reconstruct()?;
        first_fixture.unchanged();
        if first_review.states[&PolicyKind::Review] != PolicyState::Verified {
            report["complete_first_review"] = json!(first_review);
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "first Review is not Verified in paired mode",
            ));
        }
        report["complete_first_review"] =
            json!({"semantic":semantic(&first_review),"traces":first_review.traces});
        report["stage"] = json!("complete independent publication before proposal");
        let initial = f.reconstruct()?;
        report["complete_initial"] = json!({"semantic":semantic(&initial),"traces":initial.traces});
        let mut final_fixture = copied_fixture(&frozen.owned);
        final_fixture.install(frozen.proposal.clone());
        let complete_final = final_fixture.reconstruct()?;
        final_fixture.unchanged();
        report["complete_final"] =
            json!({"semantic":semantic(&complete_final),"traces":complete_final.traces});
        report["input_hashes"] = json!(hashes(&frozen.owned));
        report["proposal_hashes"] = json!(
            frozen
                .proposal
                .operations
                .iter()
                .map(|o| (
                    o.target.clone(),
                    Blake3Hash::digest(o.proposed.as_ref().unwrap())
                ))
                .collect::<BTreeMap<_, _>>()
        );
        report["proposal_writes"] = json!(frozen.proposal.operations.len());
        report["proposal_preconditions"] = json!(frozen.proposal.read_preconditions);
        assert_eq!(frozen.proposal.operations.len(), 275);
        let reader = f.catalog.query_snapshot(QueryReadLimits::default())?;
        let mut meter = GraphOperationMeter::new();
        report["stage"] = json!("second selected capture");
        let captured = capture_graph_inputs(
            &f.fs,
            &reader,
            GraphInputRequest::Resolve(&frozen.request),
            &mut meter,
        );
        report["capture_query"] =
            json!({"rows":reader.usage().rows,"bytes":reader.usage().bytes,"usage":meter.usage()});
        let query_captured = reader.usage().bytes;
        let selected = match captured {
            Ok(s) => s,
            Err(e) => {
                f.unchanged();
                report["owned_canonical_unchanged"] = json!(true);
                return Err(e);
            }
        };
        let view = selected.view_metered(&f.fs, &mut meter)?;
        validate_resolution(&view, &serde_json::to_vec(&frozen.request).unwrap())?;
        // Constructor allocations were frozen once. Reconstructing would generate
        // new Decision identities and invalidate the paired canonical-byte comparison.
        drop(view);
        let draft = &frozen.proposal;
        let targets: BTreeSet<_> = draft.operations.iter().map(|o| o.target.clone()).collect();
        let mut before = BTreeMap::new();
        let mut overlay = BTreeMap::new();
        for op in &draft.operations {
            if let Some(bytes) = f.owned.get(&op.target) {
                before.insert(op.target.clone(), parse_note(bytes));
            }
            let bytes = op.proposed.as_ref().unwrap();
            meter.charge_processing(bytes.len())?;
            overlay.insert(op.target.clone(), parse_note(bytes));
        }
        let mut changed = BTreeSet::new();
        let mut classification_bytes = 0usize;
        for (p, n) in before.iter().chain(&overlay) {
            let bytes = remap_probe_membership_bytes(n)?;
            meter.charge_processing(bytes)?;
            classification_bytes += bytes;
            changed.extend(policy_membership_keys(p, n)?);
        }
        let affected = reader.policy_affected(&changed, &targets)?;
        report["changed_membership_keys"] = json!(changed);
        report["affected_policy"] = json!(affected);
        report["diagnostic_membership_classifier_bytes"] = json!(classification_bytes);
        if population != "C" {
            assert_eq!(
                changed.contains(&PolicyInputKey::RemapReceiptCandidates),
                mode == RemapInputProbeMode::Current
            );
        }
        report["stage"] = json!("real prospective projection");
        let mut access = Access {
            fs: &f.fs,
            meter: &mut meter,
            loaded: BTreeMap::new(),
            steps: 0,
        };
        let projected = project_graph_policy_probe(&reader, &before, &overlay, &mut access);
        report["projection_work"] = json!({"loaded_paths":access.loaded,"steps":access.steps,
        "query_rows":reader.usage().rows,"query_bytes":reader.usage().bytes,"usage":access.meter.usage()});
        let loaded = access.loaded.clone();
        drop(access);
        meter.charge_processing(reader.usage().bytes.saturating_sub(query_captured))?;
        let delta = match projected {
            Ok(d) => d,
            Err(e) => {
                f.unchanged();
                report["owned_canonical_unchanged"] = json!(true);
                return Err(e);
            }
        };
        report["projection_replacements"] = json!(delta.replacements);
        let review_count = delta
            .replacements
            .iter()
            .flat_map(|r| &r.rows)
            .filter(|r| {
                matches!(
                    r,
                    PolicyRow::State {
                        kind: PolicyKind::Review,
                        ..
                    }
                )
            })
            .count();
        if mode == RemapInputProbeMode::Current {
            assert_eq!(review_count, 1);
        }
        if population != "C" && mode == RemapInputProbeMode::Precise {
            assert_eq!(review_count, 0);
            assert_eq!(initial.states[&PolicyKind::Review], PolicyState::Verified);
            assert_eq!(
                complete_final.states[&PolicyKind::Review],
                PolicyState::Verified
            );
        }
        for kind in [PolicyKind::Remap, PolicyKind::Review] {
            let replacements: Vec<_> = delta
                .replacements
                .iter()
                .filter(|r| r.kind == kind)
                .collect();
            assert!(replacements.len() <= 1);
            let actual = if let Some(r) = replacements.first() {
                replacement_output(&r.rows)
            } else {
                complete_output(&initial, kind)?
            };
            assert_eq!(
                actual,
                complete_output(&complete_final, kind)?,
                "candidate projection differs from independent complete final evaluation"
            );
        }
        report["projection_matches_complete_final"] = json!(true);
        let mut guards: BTreeMap<_, _> = selected
            .dependencies()
            .iter()
            .chain(&draft.read_preconditions)
            .filter(|d| !targets.contains(&d.path))
            .map(|d| (d.path.clone(), d.expected.clone()))
            .collect();
        for replacement in &delta.replacements {
            for row in &replacement.rows {
                if let PolicyRow::ReadPath { path, hash, .. } = row {
                    if !targets.contains(path) {
                        guards.insert(path.clone(), ExpectedState::Hash(hash.clone()));
                    }
                }
            }
        }
        for (p, h) in &loaded {
            if !targets.contains(p) {
                assert_eq!(guards.get(p), Some(&ExpectedState::Hash(h.clone())));
            }
        }
        if mode == RemapInputProbeMode::Current {
            assert!(guards.contains_key(&frozen.carrier));
        }
        assert!(guards.contains_key(&path("WIKI.md")));
        let source_paths: Vec<_> = f
            .owned
            .keys()
            .filter(|p| p.as_str().starts_with(&frozen.source_prefix))
            .collect();
        assert_eq!(
            source_paths
                .iter()
                .filter(|p| p.as_str().ends_with("/revision.md"))
                .count(),
            2
        );
        assert_eq!(
            source_paths
                .iter()
                .filter(|p| p.as_str().ends_with("/original.bin"))
                .count(),
            2
        );
        assert_eq!(
            source_paths
                .iter()
                .filter(|p| p.as_str().ends_with("/content.md"))
                .count(),
            2
        );
        for p in source_paths {
            assert!(guards.contains_key(p), "complete Source history guard {p}");
        }
        let sizes: BTreeMap<_, _> = guards
            .iter()
            .map(|(p, e)| {
                let size = match e {
                    ExpectedState::Absent => 0,
                    ExpectedState::Hash(h) => {
                        let bytes = f.owned.get(p).expect("guard is owned fixture input");
                        assert_eq!(*h, Blake3Hash::digest(bytes));
                        bytes.len()
                    }
                };
                (p.clone(), size)
            })
            .collect();
        let total = sizes.values().sum::<usize>();
        let floor = total * 3 * draft.operations.len() + meter.usage().processing_bytes;
        report["guard_floor"] = json!({"guards":guards,"sizes":sizes,"unchanged_bytes":total,"passes_per_write":3,
        "processing_already_used":meter.usage().processing_bytes,"required_bytes":floor,"budget_bytes":GRAPH_PROCESSING_BYTES,
        "query_rows":reader.usage().rows,"query_bytes":reader.usage().bytes,"usage":meter.usage(),
        "full_projector_rows":null,"full_projector":"unavailable","measurement":"lower bound only; constructor internal copies incomplete"});
        let admission = meter.preflight_guard_floor(total, draft.operations.len());
        if floor > GRAPH_PROCESSING_BYTES {
            assert_eq!(admission.unwrap_err().code, ErrorCode::BudgetExceeded);
            report["admission"] = json!("NO_GO: lower bound exceeds unchanged budget");
        } else {
            admission?;
            report["admission"] =
                json!("INCONCLUSIVE: lower bound fits; FTS/full projector unavailable");
        }
        f.unchanged();
        report["owned_canonical_unchanged"] = json!(true);
        report["stage"] = json!("complete lower-bound observation");
        Ok(())
    })();
    f.unchanged();
    report["owned_canonical_unchanged"] = json!(true);
    result
}
fn split_control(report: &mut Value) -> Result<()> {
    let mut f = Fixture::new();
    add_remap_source(&mut f);
    let quote = "Ada works for Acme in the independent genuine split control.\n".to_string();
    let span = ByteSpan::new(0, quote.len() as u64)?;
    let mut capture = capture_request(quote.clone().into_bytes());
    capture.origin = "owned-split-control.md".into();
    let source = SourceStore::new(f.fs.clone()).plan_capture(capture)?;
    let sid = source.source_id;
    let revision = source.revision_id;
    f.install(source.draft.unwrap());
    let extraction = f.import(&sid, &revision, &[span], &[vec![(span, quote)]])?;
    let mut request = f.resolve_request(&extraction)?;
    bind_remap_source(&f, &mut request)?;
    install_resolution(&mut f, &request)?;
    let request = entity_request(&f, true)?;
    report["request"] = request.clone();
    let prior = f.owned.clone();
    install_entity_request(&mut f, &request)?;
    let retained: BTreeSet<_> = f
        .owned
        .iter()
        .filter_map(|(p, b)| {
            let n = parse_note(b);
            let r = n.canonical?;
            (r.kind() == RecordKind::Entity && !prior.contains_key(p)).then(|| r.id().clone())
        })
        .collect();
    assert_eq!(retained.len(), 2);
    let view = f.view()?;
    let binding = load_extraction(&view, &extraction)?.artifact().bindings
        [&PacketLocalId::new("m_ada")?]
        .clone();
    let MentionBinding::Resolved { entity_id, .. } = binding else {
        panic!("split retained resolved entity")
    };
    assert!(retained.contains(&entity_id));
    drop(view);
    let mut semantics = vec![];
    for mode in [RemapInputProbeMode::Current, RemapInputProbeMode::Precise] {
        let policy = with_mode(mode, || copied_fixture(&f.owned).reconstruct())?;
        assert_eq!(policy.states[&PolicyKind::Remap], PolicyState::Verified);
        semantics.push(semantic(&policy));
    }
    assert_eq!(semantics[0], semantics[1]);
    f.unchanged();
    report["retained_entities"] = json!(retained);
    report["resolved_entity"] = json!(entity_id);
    report["complete_semantic_comparison"] = json!(semantics);
    report["owned_canonical_unchanged"] = json!(true);
    Ok(())
}
fn paired_population(population: &str) {
    let mut construction =
        json!({"population":population,"stage":"fixed construction","workflow_pass":false});
    let prepared = with_mode(RemapInputProbeMode::Current, || {
        freeze_workflow(population, &mut construction)
    });
    println!(
        "PRECISE_MEMBERSHIP_WORKFLOW_CONSTRUCTION {}",
        serde_json::to_string(&construction).unwrap()
    );
    let frozen = match prepared {
        Ok(f) => f,
        Err(e) => {
            construction["error"] = json!(e);
            construction["admission"] = json!("NO_GO: first history/semantic preparation failure");
            println!(
                "PRECISE_MEMBERSHIP_WORKFLOW_PREPARATION_FAILURE {}",
                serde_json::to_string(&construction).unwrap()
            );
            // Genuine history can be semantically unavailable. It receives no pass credit.
            if population != "C" {
                assert_eq!(
                    e.code,
                    ErrorCode::BudgetExceeded,
                    "unexpected preparation failure: {e}"
                );
            }
            return;
        }
    };
    let mut arms = vec![];
    for mode in [RemapInputProbeMode::Current, RemapInputProbeMode::Precise] {
        let mut report = json!({"schema":"precise-remap-membership-paired-workflow-development.v1",
                "population":population,"mode":format!("{mode:?}"),"workflow_pass":false,
                "layout":"normalized original","stage":"independent complete publication",
                "processing_measurement":"charged capture/query/policy/classifier; constructor internal copies incomplete; lower bound only"});
        let result = with_mode(mode, || paired_arm(&frozen, mode, population, &mut report));
        if let Err(e) = &result {
            report["error"] = json!(e);
            report["admission"] = json!("NO_GO: first actual measured stage failure");
        }
        println!(
            "PRECISE_MEMBERSHIP_WORKFLOW {}",
            serde_json::to_string(&report).unwrap()
        );
        if let Err(e) = result {
            assert_eq!(
                e.code,
                ErrorCode::BudgetExceeded,
                "unexpected paired arm failure: {e}"
            );
        }
        arms.push(report);
    }
    assert_eq!(
        arms[0]["complete_first_review"]["semantic"],
        arms[1]["complete_first_review"]["semantic"]
    );
    assert_eq!(
        arms[0]["complete_initial"]["semantic"],
        arms[1]["complete_initial"]["semantic"]
    );
    assert_eq!(
        arms[0]["complete_final"]["semantic"],
        arms[1]["complete_final"]["semantic"]
    );
}

#[test]
fn dev_precise_membership_no_remap_full_workflow() {
    paired_population("A");
}
#[test]
fn dev_precise_membership_ordinary_history_full_workflow() {
    paired_population("B");
}
#[test]
fn dev_precise_membership_genuine_merge_full_workflow() {
    paired_population("C");
}
#[test]
fn dev_precise_membership_genuine_split_control() {
    let mut report = json!({"schema":"precise-remap-membership-genuine-split-development.v1","stage":"genuine split construction"});
    let result = with_mode(RemapInputProbeMode::Current, || split_control(&mut report));
    if let Err(e) = &result {
        report["error"] = json!(e);
        report["admission"] = json!("NO_GO: genuine split control failed");
    }
    println!(
        "PRECISE_MEMBERSHIP_SPLIT_CONTROL {}",
        serde_json::to_string(&report).unwrap()
    );
    result.expect("genuine split semantic control");
}

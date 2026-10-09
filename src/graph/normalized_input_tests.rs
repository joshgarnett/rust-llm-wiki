use super::*;
use crate::{
    catalog::query_types::SourceRevisionTuple,
    sources::revision::{common, record_bytes},
    vault::VaultRoot,
};
use serde_json::{Value, json};

fn id(s: &str) -> RecordId {
    RecordId::new(s).unwrap()
}
fn path(s: &str) -> VaultRelativePath {
    VaultRelativePath::new(s).unwrap()
}
fn bytes(kind: RecordKind, name: &str, extra: Value, body: &[u8]) -> Vec<u8> {
    let mut fields = common(&id(name), kind, name);
    fields.extend(
        extra
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone())),
    );
    record_bytes(CanonicalRecord::new(fields).unwrap(), body).unwrap()
}
struct MockCatalog {
    snapshot: ReadSnapshot,
    vault: RecordId,
    claims: BTreeMap<RecordId, Vec<IdentityClaimRow>>,
    rows: BTreeMap<RecordId, RecordRow>,
    edges: Vec<EligibilityEdge>,
    tuples: BTreeMap<(RecordId, RecordId), SourceRevisionTuple>,
    direct: BTreeMap<RecordId, BTreeMap<VaultRelativePath, ExpectedState>>,
    policy: BTreeMap<PolicyInputKey, Vec<(VaultRelativePath, Blake3Hash)>>,
    affected: BTreeSet<PolicyKind>,
    failed_key: Option<PolicyInputKey>,
}
impl CaptureCatalog for MockCatalog {
    fn snapshot(&self) -> &ReadSnapshot {
        &self.snapshot
    }
    fn vault_id(&self) -> &RecordId {
        &self.vault
    }
    fn check(&self) -> Result<()> {
        Ok(())
    }
    fn query_bytes(&self) -> usize {
        0
    }
    fn claims(&self, id: &RecordId) -> Result<Vec<IdentityClaimRow>> {
        Ok(self.claims.get(id).cloned().unwrap_or_default())
    }
    fn record(&self, id: &RecordId) -> Result<Option<RecordRow>> {
        Ok(self.rows.get(id).cloned())
    }
    fn edges(
        &self,
        id: &RecordId,
        roles: &[EligibilityRole],
        reverse: bool,
    ) -> Result<Vec<EligibilityEdge>> {
        Ok(self
            .edges
            .iter()
            .filter(|e| {
                roles.contains(&e.role) && (if reverse { &e.target_id } else { &e.owner_id }) == id
            })
            .cloned()
            .collect())
    }
    fn policy(&self, key: &PolicyInputKey) -> Result<Vec<(VaultRelativePath, Blake3Hash)>> {
        if self.failed_key.as_ref() == Some(key) {
            return Err(WikiError::new(
                ErrorCode::OfflineUnavailable,
                "missing published policy key layout",
            ));
        }
        Ok(self.policy.get(key).cloned().unwrap_or_default())
    }
    fn affected(
        &self,
        _: &BTreeSet<PolicyInputKey>,
        _: &BTreeSet<VaultRelativePath>,
    ) -> Result<BTreeSet<PolicyKind>> {
        Ok(self.affected.clone())
    }
    fn revision_tuple(
        &self,
        source: &RecordId,
        revision: &RecordId,
    ) -> Result<Option<SourceRevisionTuple>> {
        Ok(self
            .tuples
            .get(&(source.clone(), revision.clone()))
            .cloned())
    }
    fn direct_states(&self, id: &RecordId) -> Result<BTreeMap<VaultRelativePath, ExpectedState>> {
        Ok(self.direct.get(id).cloned().unwrap_or_default())
    }
}
struct Fixture {
    temp: tempfile::TempDir,
    fs: VaultFs,
    catalog: MockCatalog,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let fs = VaultFs::new(VaultRoot::for_initialization(temp.path()).unwrap());
        let mut f = Self {
            temp,
            fs,
            catalog: MockCatalog {
                snapshot: ReadSnapshot {
                    generation: 1,
                    parser_fingerprint: Blake3Hash::digest(b"parser"),
                    binding: SnapshotBinding::PublishedEpoch {
                        publication: PublishedEpochBinding {
                            version: 1,
                            file_id: "a".repeat(32),
                            publication_hash: Blake3Hash::digest(b"publication"),
                        },
                    },
                },
                vault: id("vault_selected"),
                claims: BTreeMap::new(),
                rows: BTreeMap::new(),
                edges: vec![],
                tuples: BTreeMap::new(),
                direct: BTreeMap::new(),
                policy: BTreeMap::new(),
                affected: BTreeSet::new(),
                failed_key: None,
            },
        };
        f.adopt(
            "WIKI.md",
            bytes(
                RecordKind::Vault,
                "vault_selected",
                json!({}),
                b"Fixture.\n",
            ),
        );
        f.adopt("sources/source_selected/source.md", bytes(RecordKind::Source, "source_selected", json!({
            "wiki_status":"active", "wiki_origin_kind":"local-file", "wiki_origin":"public-dev.txt",
            "wiki_current_revision":"revision_one", "wiki_revisions":["revision_zero","revision_one"],
            "wiki_revision":"[[sources/source_selected/revisions/revision_one/revision.md]]"
        }), b"Selected fixture.\n"));
        for (ordinal, revision, text) in [
            (0, "revision_zero", b"zero".as_slice()),
            (1, "revision_one", b"one!".as_slice()),
        ] {
            let parent = format!("sources/source_selected/revisions/{revision}");
            let hash = Blake3Hash::digest(text);
            let extractor = Blake3Hash::digest(b"utf8");
            f.adopt(&format!("{parent}/revision.md"), bytes(RecordKind::Revision, revision, json!({
                "wiki_source_id":"source_selected", "wiki_source":"[[sources/source_selected/source.md]]",
                "wiki_original_path":"original.bin", "wiki_original_hash":hash,
                "wiki_captured_at":"2026-10-09T00:00:00Z", "wiki_extractor":"utf8", "wiki_extractor_fingerprint":extractor,
                "wiki_extraction_status":"complete", "wiki_content_path":"content.md", "wiki_content_hash":hash
            }), b""));
            f.write(&format!("{parent}/original.bin"), text);
            f.write(&format!("{parent}/content.md"), text);
            f.catalog.tuples.insert(
                (id("source_selected"), id(revision)),
                SourceRevisionTuple {
                    source_id: id("source_selected"),
                    revision_id: id(revision),
                    retained_ordinal: ordinal,
                    original_hash: hash.clone(),
                    content_hash: Some(hash),
                    extractor_fingerprint: extractor,
                    extraction_status: "complete".into(),
                },
            );
        }
        f
    }
    fn write(&self, relative: &str, data: &[u8]) {
        let target = self.temp.path().join(relative);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, data).unwrap();
    }
    fn adopt(&mut self, relative: &str, data: Vec<u8>) {
        self.write(relative, &data);
        let note = parse_note(&data);
        let record = note.canonical.unwrap();
        let p = path(relative);
        let claim = IdentityClaimRow {
            id: record.id().clone(),
            path: p.clone(),
            hash: note.source_hash.clone(),
            kind: Some(record.kind()),
        };
        self.catalog.claims.insert(record.id().clone(), vec![claim]);
        self.catalog.rows.insert(
            record.id().clone(),
            RecordRow {
                record,
                path: p,
                hash: note.source_hash,
                authored_status: None,
                eligibility: Eligibility::Current,
                reasons: vec![],
                identity_eligibility: None,
                description_eligibility: None,
                disputed: false,
                dependencies: vec![],
            },
        );
    }
    fn request(&self) -> ExportRequest {
        ExportRequest {
            source_id: id("source_selected"),
            revision_id: Some(id("revision_one")),
            windows: vec![],
            limits: ExtractionLimits::default(),
            candidate_context: vec![],
        }
    }
    fn packet(&self, meter: &mut GraphOperationMeter) -> Result<SelectedGraphInput> {
        capture(
            &self.fs,
            &self.catalog,
            GraphInputRequest::Packet(&self.request()),
            meter,
        )
    }
    fn review(&mut self) -> ReviewRequest {
        self.adopt(
            "knowledge/entities/entity_selected.md",
            bytes(
                RecordKind::Entity,
                "entity_selected",
                json!({"wiki_status":"active","wiki_entity_type":"concept"}),
                b"Explicit entity.\n",
            ),
        );
        self.adopt("knowledge/assertions/assertion_selected.md", bytes(RecordKind::Assertion,"assertion_selected",json!({"wiki_status":"proposed","wiki_subject_id":"entity_selected","wiki_object_id":"entity_selected","wiki_predicate":"uses"}),b"Proposed.\n"));
        let mut checks = vec![];
        for (e, revision, text) in [
            ("evidence_zero", "revision_zero", b"zero".as_slice()),
            ("evidence_one", "revision_one", b"one!".as_slice()),
        ] {
            self.adopt(&format!("knowledge/evidence/{e}.md"), bytes(RecordKind::Evidence,e,json!({
                "wiki_status":"active", "wiki_assertion_id":"assertion_selected", "wiki_source_id":"source_selected",
                "wiki_source_revision":revision, "wiki_stance":"supports", "wiki_locator_kind":"utf8-bytes", "wiki_span_start":0, "wiki_span_end":4,
                "wiki_quote_hash":Blake3Hash::digest(text)
            }),format!("\n## Quote\n\n{}\n",std::str::from_utf8(text).unwrap()).as_bytes()));
            self.catalog.edges.push(EligibilityEdge {
                owner_id: id(e),
                target_id: id("assertion_selected"),
                role: typed("wiki_assertion_id"),
            });
            self.catalog.edges.push(EligibilityEdge {
                owner_id: id("assertion_selected"),
                target_id: id(e),
                role: EligibilityRole::AssertionEvidence,
            });
            checks.push(EvidenceCheck {
                evidence_id: id(e),
                expected_hash: self.catalog.rows[&id(e)].hash.clone(),
                assessment: EvidenceAssessment::Supports,
            });
        }
        ReviewRequest {
            schema: GRAPH_REVIEW_SCHEMA.into(),
            decisions: vec![AssertionReview {
                assertion_id: id("assertion_selected"),
                expected_hash: self.catalog.rows[&id("assertion_selected")].hash.clone(),
                decision: ReviewDecision::Accept,
                reason: "Complete public DEV checks".into(),
                evidence_checks: checks,
            }],
            supersedes: vec![],
        }
    }
}

#[test]
fn selected_history_is_complete_and_unrelated_disk_files_stay_outside_capture() {
    let f = Fixture::new();
    f.write(
        "knowledge/pages/unrelated.md",
        b"not canonical; untouched outside published keys",
    );
    let mut meter = GraphOperationMeter::new();
    let mut selected = f.packet(&mut meter).unwrap();
    assert_eq!(selected.dependencies().len(), 8);
    assert_eq!(meter.usage().captured_files, 8);
    assert_eq!(selected.family(), GraphOperationFamily::PacketPersist);
    assert!(
        !selected
            .validation_input()
            .documents
            .iter()
            .any(|d| d.path.as_str().contains("unrelated"))
    );
    let current = path("sources/source_selected/revisions/revision_one/content.md");
    selected.input.documents.retain(|d| d.path != current);
    // Matching bytes remain on disk. The deliberately incomplete closed view
    // still refuses, proving there is no filesystem fallback in its verifier.
    let view = selected.view_metered(&f.fs, &mut meter).unwrap();
    assert_eq!(
        view.expected_state(&path("knowledge/pages/unrelated.md"))
            .unwrap_err()
            .code,
        ErrorCode::SourceIntegrity
    );
    assert_eq!(
        view.revision_content(
            &id("source_selected"),
            &id("revision_one"),
            &mut BTreeMap::new()
        )
        .unwrap_err()
        .code,
        ErrorCode::SourceIntegrity
    );
}

#[test]
fn retained_sibling_tamper_missing_asset_and_wrong_ordinal_refuse() {
    for case in 0..3 {
        let mut f = Fixture::new();
        match case {
            0 => f.write(
                "sources/source_selected/revisions/revision_zero/original.bin",
                b"evil",
            ),
            1 => std::fs::remove_file(
                f.temp
                    .path()
                    .join("sources/source_selected/revisions/revision_zero/content.md"),
            )
            .unwrap(),
            2 => {
                f.catalog
                    .tuples
                    .get_mut(&(id("source_selected"), id("revision_zero")))
                    .unwrap()
                    .retained_ordinal = 1
            }
            _ => unreachable!(),
        }
        assert!(f.packet(&mut GraphOperationMeter::new()).is_err());
    }
}

#[test]
fn duplicate_malformed_identity_claims_and_root_wiki_changes_refuse() {
    let mut f = Fixture::new();
    let raw = b"---\nwiki_schema: 1\nwiki_kind: source\nwiki_id: source_selected\ntitle: Duplicate\n---\nMalformed source.\n";
    f.write("knowledge/pages/duplicate.md", raw);
    f.catalog
        .claims
        .get_mut(&id("source_selected"))
        .unwrap()
        .push(IdentityClaimRow {
            id: id("source_selected"),
            path: path("knowledge/pages/duplicate.md"),
            hash: Blake3Hash::digest(raw),
            kind: None,
        });
    assert_eq!(
        f.packet(&mut GraphOperationMeter::new())
            .err()
            .unwrap()
            .code,
        ErrorCode::ReferenceAmbiguous
    );
    let mut malformed = Fixture::new();
    let source_path = path("sources/source_selected/source.md");
    malformed.write(source_path.as_str(), raw);
    malformed.catalog.claims.insert(
        id("source_selected"),
        vec![IdentityClaimRow {
            id: id("source_selected"),
            path: source_path,
            hash: Blake3Hash::digest(raw),
            kind: None,
        }],
    );
    malformed.catalog.rows.remove(&id("source_selected"));
    assert_eq!(
        malformed
            .packet(&mut GraphOperationMeter::new())
            .err()
            .unwrap()
            .code,
        ErrorCode::IndexCorrupt
    );
    let f = Fixture::new();
    f.write(
        "WIKI.md",
        &bytes(RecordKind::Vault, "vault_other", json!({}), b"Foreign.\n"),
    );
    assert_eq!(
        f.packet(&mut GraphOperationMeter::new())
            .err()
            .unwrap()
            .code,
        ErrorCode::ContentConflict
    );
}

#[test]
fn review_never_clips_an_intersecting_predecessors_full_scope() {
    let mut f = Fixture::new();
    let mut request = f.review();
    f.adopt("knowledge/decisions/decision_prior.md", bytes(RecordKind::Decision,"decision_prior",json!({
        "wiki_status":"active", "wiki_action":"accept", "wiki_created_at":"2026-10-09T00:00:00Z",
        "wiki_input_ids":["assertion_selected","assertion_outside"], "wiki_output_ids":["assertion_selected","assertion_outside"]
    }),b"Explicit full scope.\n"));
    let prior = &f.catalog.rows[&id("decision_prior")];
    request
        .supersedes
        .push(super::super::decision_types::ExpectedRecord {
            record_id: id("decision_prior"),
            hash: prior.hash.clone(),
        });
    f.catalog.policy.insert(
        PolicyInputKey::ActiveDecisionOutput(id("assertion_selected")),
        vec![(prior.path.clone(), prior.hash.clone())],
    );
    assert_eq!(
        capture(
            &f.fs,
            &f.catalog,
            GraphInputRequest::Review(&request),
            &mut GraphOperationMeter::new()
        )
        .err()
        .unwrap()
        .code,
        ErrorCode::ContentConflict
    );
}

#[test]
fn review_requires_all_active_evidence_and_both_exact_membership_relations() {
    let mut f = Fixture::new();
    let mut request = f.review();
    let selected = capture(
        &f.fs,
        &f.catalog,
        GraphInputRequest::Review(&request),
        &mut GraphOperationMeter::new(),
    )
    .unwrap();
    assert_eq!(
        selected
            .evidence_members(&id("assertion_selected"))
            .unwrap()
            .len(),
        2
    );
    assert!(
        selected
            .certificates()
            .contains(&PolicyInputKey::ReviewReceiptCandidates)
    );
    request.decisions[0].evidence_checks.pop();
    assert_eq!(
        capture(
            &f.fs,
            &f.catalog,
            GraphInputRequest::Review(&request),
            &mut GraphOperationMeter::new()
        )
        .err()
        .unwrap()
        .code,
        ErrorCode::ContentConflict
    );
    let request = f.review();
    f.catalog.edges.retain(|e| {
        !(e.role == EligibilityRole::AssertionEvidence && e.target_id == id("evidence_one"))
    });
    assert_eq!(
        capture(
            &f.fs,
            &f.catalog,
            GraphInputRequest::Review(&request),
            &mut GraphOperationMeter::new()
        )
        .err()
        .unwrap()
        .code,
        ErrorCode::IndexCorrupt
    );
}

#[test]
fn unavailable_policy_certificate_is_never_empty_and_shared_meter_never_resets() {
    let mut f = Fixture::new();
    let review = f.review();
    f.catalog.failed_key = Some(PolicyInputKey::ReviewReceiptCandidates);
    assert_eq!(
        capture(
            &f.fs,
            &f.catalog,
            GraphInputRequest::Review(&review),
            &mut GraphOperationMeter::new()
        )
        .err()
        .unwrap()
        .code,
        ErrorCode::OfflineUnavailable
    );
    let f = Fixture::new();
    let mut meter = GraphOperationMeter::new();
    meter
        .charge_processing(super::super::normalized_meter::GRAPH_PROCESSING_BYTES - 1)
        .unwrap();
    assert_eq!(
        f.packet(&mut meter).err().unwrap().code,
        ErrorCode::BudgetExceeded
    );
    assert!(
        meter.usage().processing_bytes
            >= super::super::normalized_meter::GRAPH_PROCESSING_BYTES - 1
    );
}

#[test]
fn published_absence_requires_actual_named_probe_and_unknown_occupied_path_refuses() {
    let mut f = Fixture::new();
    let optional = path("sources/source_selected/revisions/revision_zero/optional.bin");
    f.catalog.direct.insert(
        id("revision_zero"),
        BTreeMap::from([(optional.clone(), ExpectedState::Absent)]),
    );
    let selected = f.packet(&mut GraphOperationMeter::new()).unwrap();
    assert!(
        selected
            .dependencies()
            .iter()
            .any(|d| d.path == optional && d.expected == ExpectedState::Absent)
    );
    assert_eq!(
        selected
            .view(&f.fs)
            .unwrap()
            .expected_state(&optional)
            .unwrap(),
        ExpectedState::Absent
    );
    f.write(
        optional.as_str(),
        b"unknown occupied file, without an adopted record",
    );
    assert_eq!(
        f.packet(&mut GraphOperationMeter::new())
            .err()
            .unwrap()
            .code,
        ErrorCode::ContentConflict
    );
}

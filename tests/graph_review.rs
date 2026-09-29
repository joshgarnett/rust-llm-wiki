#[path = "fixtures/p13/support.rs"]
mod support;
#[path = "../test_support/paths.rs"]
mod test_paths;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    fs::File,
    io,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use support::*;

fn request(f: &Fixture, a: &RecordId, decision: &str, assessment: &str) -> Value {
    let (_, an) = note(f, a);
    let mut checks = vec![];
    for p in f.root.scan_markdown().unwrap() {
        let n = lwiki::records::parse_note(&fs::read(f.root.path().join(p.as_str())).unwrap());
        if let Some(r) = &n.canonical
            && r.kind() == RecordKind::Evidence
            && r.string("wiki_assertion_id") == Some(a.as_str())
            && r.string("wiki_status") == Some("active")
        {
            checks.push(
                json!({"evidence_id":r.id(),"expected_hash":n.source_hash,"assessment":assessment}),
            );
        }
    }
    json!({"schema":GRAPH_REVIEW_SCHEMA,"decisions":[{"assertion_id":a,"expected_hash":an.source_hash,"decision":decision,"reason":"Explicit complete fixture review","evidence_checks":checks}]})
}
fn review_stage(f: &Fixture, r: &Value) -> Result<ReviewOutcome> {
    let v = validate_review(&f.view(), &serde_json::to_vec(r).unwrap())?;
    stage_review(&f.engine, &f.writer(), &v)
}
fn review_apply(f: &Fixture, r: &Value) -> ReviewOutcome {
    let out = review_stage(f, r).unwrap();
    f.apply_prepared(out.prepared.as_ref().unwrap());
    out
}
fn supersede(f: &Fixture, r: &mut Value, old: &ReviewOutcome) {
    r["supersedes"] = json!(
        old.allocations
            .decisions
            .values()
            .map(|id| {
                let (_, n) = note(f, id);
                json!({"record_id":id,"hash":n.source_hash})
            })
            .collect::<Vec<_>>()
    );
}
fn extra_evidence(f: &Fixture, a: &RecordId, stance: EvidenceStance) -> RecordId {
    extra_evidence_mode(f, a, stance, false)
}
fn extra_evidence_mode(
    f: &Fixture,
    a: &RecordId,
    stance: EvidenceStance,
    direct: bool,
) -> RecordId {
    let req = request(f, a, "accept", "supports");
    let eid = id(req["decisions"][0]["evidence_checks"][0]["evidence_id"]
        .as_str()
        .unwrap());
    let (_, n) = note(f, &eid);
    let r = n.canonical.unwrap();
    let reference = EvidenceRef {
        evidence_id: eid,
        assertion_id: a.clone(),
        source_id: id(r.string("wiki_source_id").unwrap()),
        source_revision: id(r.string("wiki_source_revision").unwrap()),
        span: ByteSpan::new(
            r.field("wiki_span_start").unwrap().as_u64().unwrap(),
            r.field("wiki_span_end").unwrap().as_u64().unwrap(),
        )
        .unwrap(),
        quote_hash: Blake3Hash::new(r.string("wiki_quote_hash").unwrap()).unwrap(),
    };
    let verified = f
        .view()
        .verify(
            &CitationRef::Assertion(reference.clone()),
            CitationScope::Historical,
        )
        .unwrap();
    let plan = SourceStore::new(f.engine.fs().clone())
        .plan_evidence(EvidenceRequest {
            assertion_id: a.clone(),
            source_id: reference.source_id,
            revision_id: reference.source_revision,
            quote: verified.quote,
            window: reference.span,
            stance,
            explanation: "A separately assessed exact quotation.".into(),
            title: "Additional evidence".into(),
        })
        .unwrap();
    let eid = plan.evidence_id;
    if direct {
        for op in plan.draft.operations {
            let path = f.root.path().join(op.target.as_str());
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, op.proposed.unwrap()).unwrap();
        }
    } else {
        f.apply(plan.draft);
    }
    eid
}
fn withdraw(f: &Fixture) {
    let p = SourceStore::new(f.engine.fs().clone())
        .plan_withdraw(&f.source, "Fixture withdrawal")
        .unwrap();
    f.apply(p.draft.unwrap());
}
fn edit(f: &Fixture, id: &RecordId, fields: BTreeMap<String, Value>, body: Option<&[u8]>) {
    let (p, n) = note(f, id);
    let bytes = lwiki::records::edit_note(&n, &fields, body, &n.source_hash).unwrap();
    f.apply(ChangeDraft {
        title: "Authorized fixture prose".into(),
        origin: None,
        inverse_of: None,
        allocated_ids: BTreeMap::new(),
        read_preconditions: vec![],
        operations: vec![ExpectedWrite {
            target: p,
            expected: ExpectedState::Hash(n.source_hash),
            proposed: Some(bytes),
            apply_after: vec![],
        }],
    });
}

#[test]
fn review_omitted_or_new_active_evidence_conflicts() {
    let f = resolved();
    let a = assertion(&f, "a1");
    let r = request(&f, &a, "accept", "supports");
    let mut omitted = r.clone();
    omitted["decisions"][0]["evidence_checks"] = json!([]);
    assert!(review_stage(&f, &omitted).is_err());
    let out = review_stage(&f, &r).unwrap();
    extra_evidence(&f, &a, EvidenceStance::Supports);
    let before = canonical_bytes(&f);
    assert!(
        f.engine
            .apply(
                &f.writer(),
                out.prepared.as_ref().unwrap(),
                &CatalogGraphValidator,
                &f.catalog
            )
            .is_err()
    );
    assert_eq!(canonical_bytes(&f), before);
    let mut duplicate = request(&f, &a, "accept", "supports");
    let c = duplicate["decisions"][0]["evidence_checks"][0].clone();
    duplicate["decisions"][0]["evidence_checks"]
        .as_array_mut()
        .unwrap()
        .push(c);
    assert!(review_stage(&f, &duplicate).is_err());
}
#[test]
fn changed_stance_successor_and_insufficient_retraction_atomic() {
    let f = resolved();
    let a = assertion(&f, "a1");
    let original = request(&f, &a, "reject", "contradicts");
    let eid = id(
        original["decisions"][0]["evidence_checks"][0]["evidence_id"]
            .as_str()
            .unwrap(),
    );
    let (_, old) = note(&f, &eid);
    let extra = extra_evidence(&f, &a, EvidenceStance::Supports);
    let mut r = request(&f, &a, "reject", "contradicts");
    for c in r["decisions"][0]["evidence_checks"].as_array_mut().unwrap() {
        if c["evidence_id"] == json!(extra) {
            c["assessment"] = json!("insufficient");
        }
    }
    let v = validate_review(&f.view(), &serde_json::to_vec(&r).unwrap()).unwrap();
    let out = stage_review(&f.engine, &f.writer(), &v).unwrap();
    let repeat = stage_review(&f.engine, &f.writer(), &v).unwrap();
    assert!(repeat.reused);
    assert_eq!(out.allocations, repeat.allocations);
    assert_eq!(out.allocations.successors.len(), 1);
    f.apply_prepared(out.prepared.as_ref().unwrap());
    let sid = &out.allocations.successors[&eid];
    let (_, successor) = note(&f, sid);
    let sr = successor.canonical.as_ref().unwrap();
    assert_eq!(successor.body(), old.body());
    assert_eq!(sr.string("wiki_stance"), Some("contradicts"));
    assert_eq!(sr.string("wiki_supersedes_id"), Some(eid.as_str()));
    assert_eq!(
        note(&f, &eid).1.canonical.unwrap().string("wiki_status"),
        Some("retracted")
    );
    assert_eq!(
        note(&f, &extra).1.canonical.unwrap().string("wiki_status"),
        Some("retracted")
    );
    for k in [
        "wiki_assertion_id",
        "wiki_source_id",
        "wiki_source_revision",
        "wiki_span_start",
        "wiki_span_end",
        "wiki_quote_hash",
        "wiki_extraction_id",
    ] {
        assert_eq!(sr.field(k), old.canonical.as_ref().unwrap().field(k));
    }
    assert!(load_extraction(&f.view(), &f.extraction).is_ok());
    let restored = stage_review(&f.engine, &f.writer(), &v).unwrap();
    assert!(restored.reused);
    assert_eq!(restored.allocations, out.allocations);
}
#[test]
fn accept_requires_post_review_current_support() {
    let f = resolved();
    let a = assertion(&f, "a1");
    for assessment in ["insufficient", "contradicts"] {
        let before = canonical_bytes(&f);
        assert!(review_stage(&f, &request(&f, &a, "accept", assessment)).is_err());
        assert_eq!(canonical_bytes(&f), before);
    }
    let rejected = review_apply(&f, &request(&f, &a, "reject", "contradicts"));
    let mut accept = request(&f, &a, "accept", "supports");
    assert!(review_stage(&f, &accept).is_err());
    supersede(&f, &mut accept, &rejected);
    let accepted = review_apply(&f, &accept);
    assert_eq!(
        note(&f, &a).1.canonical.unwrap().string("wiki_status"),
        Some("accepted")
    );
    assert_eq!(accepted.allocations.successors.len(), 1);
    let again = request(&f, &a, "accept", "supports");
    assert!(review_stage(&f, &again).is_err());
    let mut explicit = again;
    supersede(&f, &mut explicit, &accepted);
    withdraw(&f);
    assert!(review_stage(&f, &explicit).is_err());
}
#[test]
fn review_cannot_restore_withdrawn_or_old_revision_support() {
    for withdrawal in [false, true] {
        let f = resolved();
        let a = assertion(&f, "a1");
        let out = review_stage(&f, &request(&f, &a, "accept", "supports")).unwrap();
        if withdrawal {
            withdraw(&f)
        } else {
            let plan = SourceStore::new(f.engine.fs().clone())
                .plan_refresh(
                    &f.source,
                    CaptureRequest {
                        title: "Successor source".into(),
                        origin_kind: SourceOrigin::LocalFile,
                        origin: "disposable.md".into(),
                        original: "Ada works for Acme.\r\nUpdated capture.\r\n"
                            .as_bytes()
                            .to_vec(),
                        extraction: ExtractionInput::Utf8Preserve,
                        media_type: Some("text/markdown".into()),
                    },
                )
                .unwrap();
            f.apply(plan.draft.unwrap());
        }
        let before = canonical_bytes(&f);
        assert!(
            f.engine
                .apply(
                    &f.writer(),
                    out.prepared.as_ref().unwrap(),
                    &CatalogGraphValidator,
                    &f.catalog
                )
                .is_err()
        );
        assert_eq!(canonical_bytes(&f), before);
        assert!(review_stage(&f, &request(&f, &a, "accept", "supports")).is_err());
    }
}
#[test]
fn review_strict_schema_limits_and_read_only_plan() {
    let f = resolved();
    let a = assertion(&f, "a1");
    let valid = request(&f, &a, "accept", "supports");
    let request_schema: Value =
        serde_json::from_str(include_str!("../schemas/graph-review-v1.json")).unwrap();
    let receipt_schema: Value =
        serde_json::from_str(include_str!("../schemas/graph-review-receipt-v1.json")).unwrap();
    assert!(jsonschema::meta::is_valid(&request_schema));
    assert!(jsonschema::meta::is_valid(&receipt_schema));
    let request_validator = jsonschema::options()
        .offline()
        .should_validate_formats(true)
        .build(&request_schema)
        .unwrap();
    let receipt_validator = jsonschema::options()
        .offline()
        .should_validate_formats(true)
        .build(&receipt_schema)
        .unwrap();
    request_validator.validate(&valid).unwrap();
    let before = canonical_bytes(&f);
    let v = validate_review(&f.view(), &serde_json::to_vec(&valid).unwrap()).unwrap();
    assert_eq!(plan_review(&v).unwrap().summary.accept_assertions, 1);
    assert_eq!(before, canonical_bytes(&f));
    for (pointer, value) in [
        ("/supersedes", Value::Null),
        ("/decisions/0/decision", json!("accepted")),
        ("/decisions/0/reason", json!("")),
        ("/decisions/0/confidence", json!(1)),
        ("/decisions/0/evidence_checks/0/extra", json!(true)),
    ] {
        let mut r = valid.clone();
        if pointer == "/supersedes" {
            r["supersedes"] = value
        } else {
            let parts = pointer.rsplit_once('/').unwrap();
            if let Some(parent) = r.pointer_mut(parts.0) {
                parent[parts.1] = value;
            }
        }
        assert!(
            validate_review(&f.view(), &serde_json::to_vec(&r).unwrap()).is_err(),
            "{pointer}"
        );
        assert!(!request_validator.is_valid(&r), "schema {pointer}");
    }
    let duplicate = format!(
        "{{\"schema\":\"{}\",\"schema\":\"{}\",\"decisions\":[]}}",
        GRAPH_REVIEW_SCHEMA, GRAPH_REVIEW_SCHEMA
    );
    assert!(validate_review(&f.view(), duplicate.as_bytes()).is_err());
    let out = review_apply(&f, &valid);
    let (_, n) = note(&f, out.allocations.decisions.values().next().unwrap());
    let mut receipt = serde_json::to_value(fixture_receipt(&n)).unwrap();
    receipt_validator.validate(&receipt).unwrap();
    receipt["created_at"] = json!("invalid-date");
    assert!(!receipt_validator.is_valid(&receipt));
    let mut many = valid;
    many["decisions"] = json!(vec![many["decisions"][0].clone(); 17]);
    assert!(validate_review(&f.view(), &serde_json::to_vec(&many).unwrap()).is_err());
}

fn proposed(f: &Fixture, p: &PreparedChange) -> ChangeDraft {
    let c = f.engine.inspect(&p.change_id).unwrap();
    ChangeDraft {
        title: "Disposable adversarial replay".into(),
        origin: c.manifest.origin.clone(),
        inverse_of: None,
        allocated_ids: c.manifest.allocated_ids.clone(),
        read_preconditions: c.manifest.read_preconditions.clone(),
        operations: c
            .manifest
            .operations
            .iter()
            .map(|o| ExpectedWrite {
                target: o.target.clone(),
                expected: o.before.clone(),
                proposed: o
                    .after_payload
                    .as_ref()
                    .map(|p| fs::read(f.root.path().join(p.path.as_str())).unwrap()),
                apply_after: o
                    .apply_after
                    .iter()
                    .map(|index| c.manifest.operations[*index].target.clone())
                    .collect(),
            })
            .collect(),
    }
}
#[test]
fn forged_extra_decision_or_mismatched_review_origin_cannot_apply() {
    for mismatch in [false, true] {
        let f = resolved();
        let a = assertion(&f, "a1");
        let out = review_stage(&f, &request(&f, &a, "accept", "supports")).unwrap();
        let mut draft = proposed(&f, out.prepared.as_ref().unwrap());
        fs::remove_dir_all(f.root.path().join(format!(
            "changes/{}",
            out.prepared.as_ref().unwrap().change_id
        )))
        .unwrap();
        if mismatch {
            draft.origin.as_mut().unwrap().response_hash = Blake3Hash::digest(b"wrong review hash");
        } else {
            let did = RecordId::generate(RecordKind::Decision).unwrap();
            let bytes=format!("---\nwiki_schema: \"1\"\nwiki_id: {did}\nwiki_kind: decision\ntitle: Extra unrelated active decision\nwiki_status: active\nwiki_action: accept\nwiki_created_at: \"2026-09-28T00:00:00Z\"\nwiki_input_ids: [{a}]\nwiki_output_ids: [{a}]\n---\nThis valid-looking write was not requested.\n").into_bytes();
            draft.operations.push(ExpectedWrite {
                target: record_path("decisions", &did),
                expected: ExpectedState::Absent,
                proposed: Some(bytes),
                apply_after: vec![],
            });
        }
        let forged = f.engine.prepare(&f.writer(), draft).unwrap().prepared;
        let before = canonical_bytes(&f);
        assert!(
            f.engine
                .apply(&f.writer(), &forged, &CatalogGraphValidator, &f.catalog)
                .is_err()
        );
        assert_eq!(canonical_bytes(&f), before);
    }
}
struct InterruptReviewIo {
    tripped: AtomicBool,
    remove_decision: bool,
}
impl DurableIo for InterruptReviewIo {
    fn create_stage(&self, p: &Path) -> io::Result<File> {
        NativeIo.create_stage(p)
    }
    fn open_append(&self, p: &Path) -> io::Result<File> {
        NativeIo.open_append(p)
    }
    fn truncate_file(&self, f: &File, n: u64) -> io::Result<()> {
        NativeIo.truncate_file(f, n)
    }
    fn write_stage(&self, f: &mut File, b: &[u8]) -> io::Result<()> {
        NativeIo.write_stage(f, b)
    }
    fn sync_file(&self, f: &File) -> io::Result<()> {
        NativeIo.sync_file(f)
    }
    fn replace(&self, s: &Path, t: &Path) -> io::Result<()> {
        NativeIo.replace(s, t)?;
        if !self.remove_decision
            && t.parent()
                .is_some_and(|p| p.ends_with("knowledge/assertions"))
            && !self.tripped.swap(true, Ordering::SeqCst)
        {
            return Err(io::Error::other(
                "real review post-assertion rename interruption",
            ));
        }
        Ok(())
    }
    fn remove(&self, p: &Path) -> io::Result<()> {
        NativeIo.remove(p)?;
        if self.remove_decision
            && p.parent()
                .is_some_and(|p| p.ends_with("knowledge/decisions"))
            && !self.tripped.swap(true, Ordering::SeqCst)
        {
            return Err(io::Error::other(
                "real review inverse post-Decision deletion interruption",
            ));
        }
        Ok(())
    }
    fn create_directory(&self, p: &Path) -> io::Result<()> {
        NativeIo.create_directory(p)
    }
    fn sync_directory(&self, p: &Path) -> io::Result<DirectorySync> {
        NativeIo.sync_directory(p)
    }
}
#[test]
fn partial_review_actual_scan_and_new_evidence_guard() {
    for added in [false, true] {
        let f = resolved();
        let a = assertion(&f, "a1");
        let out = review_stage(&f, &request(&f, &a, "accept", "supports")).unwrap();
        let io = Arc::new(InterruptReviewIo {
            tripped: AtomicBool::new(false),
            remove_decision: false,
        });
        let faulted = ChangeEngine::new(VaultFs::with_io(f.root.clone(), io.clone())).unwrap();
        assert!(
            faulted
                .apply(
                    &f.writer(),
                    out.prepared.as_ref().unwrap(),
                    &CatalogGraphValidator,
                    &f.catalog
                )
                .is_err()
        );
        assert!(io.tripped.load(Ordering::SeqCst));
        assert_eq!(
            note(&f, &a).1.canonical.unwrap().string("wiki_status"),
            Some("accepted")
        );
        if added {
            extra_evidence_mode(&f, &a, EvidenceStance::Supports, true);
            let before = canonical_bytes(&f);
            assert!(
                f.engine
                    .recover(&f.writer(), &CatalogGraphValidator, &f.catalog)
                    .is_err()
            );
            assert_eq!(canonical_bytes(&f), before);
        } else {
            f.engine
                .recover(&f.writer(), &CatalogGraphValidator, &f.catalog)
                .unwrap();
            assert_eq!(
                f.engine
                    .inspect(&out.prepared.unwrap().change_id)
                    .unwrap()
                    .status,
                ChangeStatus::Committed
            );
        }
    }
}
struct InterruptReviewFilesApplied {
    accepted: RecordId,
    tripped: AtomicBool,
    inverse: bool,
}
impl GraphValidator for InterruptReviewFilesApplied {
    fn validate(&self, fs: &VaultFs, input: &ValidationInput) -> Result<ValidatedGraph> {
        CatalogGraphValidator.validate(fs, input)
    }
    fn validate_retained(
        &self,
        fs: &VaultFs,
        input: &ValidationInput,
        w: &RetainedGraphInput,
    ) -> Result<ValidatedGraph> {
        let g = CatalogGraphValidator.validate_retained(fs, input, w)?;
        if !self.inverse
            && input.documents.iter().any(|d| {
                lwiki::records::parse_note(&d.bytes)
                    .canonical
                    .as_ref()
                    .is_some_and(|r| {
                        r.id() == &self.accepted && r.string("wiki_status") == Some("accepted")
                    })
            })
            && !self.tripped.swap(true, Ordering::SeqCst)
        {
            return Err(WikiError::new(
                ErrorCode::Internal,
                "real GraphReview FilesApplied validation interruption",
            ));
        }
        Ok(g)
    }
    fn validate_inverse(
        &self,
        fs: &VaultFs,
        input: &ValidationInput,
        w: &RetainedGraphInverseInput,
    ) -> Result<ValidatedGraph> {
        let p = verify_review_inverse_overlay(fs, input, w)?;
        assert!(p.restored_accepted().contains(&self.accepted));
        let g = CatalogGraphValidator.validate_inverse(fs, input, w)?;
        if self.inverse
            && input.documents.iter().any(|d| {
                lwiki::records::parse_note(&d.bytes)
                    .canonical
                    .as_ref()
                    .is_some_and(|r| {
                        r.id() == &self.accepted && r.string("wiki_status") == Some("accepted")
                    })
            })
            && !self.tripped.swap(true, Ordering::SeqCst)
        {
            return Err(WikiError::new(
                ErrorCode::Internal,
                "real review inverse FilesApplied validation interruption",
            ));
        }
        Ok(g)
    }
}
#[test]
fn already_accepted_files_applied_recovery_rechecks_current_support() {
    for withdrawn in [false, true] {
        let f = resolved();
        let a = assertion(&f, "a1");
        let out = review_stage(&f, &request(&f, &a, "accept", "supports")).unwrap();
        let validator = InterruptReviewFilesApplied {
            accepted: a.clone(),
            tripped: AtomicBool::new(false),
            inverse: false,
        };
        assert!(
            f.engine
                .apply(
                    &f.writer(),
                    out.prepared.as_ref().unwrap(),
                    &validator,
                    &f.catalog
                )
                .is_err()
        );
        assert!(validator.tripped.load(Ordering::SeqCst));
        assert_eq!(
            f.engine
                .inspect(&out.prepared.as_ref().unwrap().change_id)
                .unwrap()
                .status,
            ChangeStatus::FilesApplied
        );
        if withdrawn {
            let (p, n) = note(&f, &f.source);
            let bytes = lwiki::records::edit_note(
                &n,
                &BTreeMap::from([
                    ("wiki_status".into(), json!("withdrawn")),
                    (
                        "wiki_withdrawal_reason".into(),
                        json!("External lifecycle during recovery"),
                    ),
                    ("wiki_withdrawn_at".into(), json!("2026-09-28T00:00:00Z")),
                ]),
                None,
                &n.source_hash,
            )
            .unwrap();
            fs::write(f.root.path().join(p.as_str()), bytes).unwrap();
            assert!(
                f.engine
                    .recover(&f.writer(), &CatalogGraphValidator, &f.catalog)
                    .is_err()
            );
        } else {
            f.engine
                .recover(&f.writer(), &CatalogGraphValidator, &f.catalog)
                .unwrap();
        }
    }
}
#[test]
fn review_inverse_undo_partial_and_stale_accepted_restoration() {
    let f = resolved();
    let a = assertion(&f, "a1");
    let accepted = review_apply(&f, &request(&f, &a, "accept", "supports"));
    let mut reject = request(&f, &a, "reject", "contradicts");
    supersede(&f, &mut reject, &accepted);
    let before = canonical_bytes(&f);
    let rejected = review_apply(&f, &reject);
    let after = canonical_bytes(&f);
    let inverse = f
        .engine
        .prepare_inverse(
            &f.writer(),
            rejected.prepared.as_ref().unwrap(),
            &CatalogGraphValidator,
        )
        .unwrap();
    f.apply_prepared(&inverse);
    assert_eq!(canonical_bytes(&f), before);
    let undo = f
        .engine
        .prepare_inverse(&f.writer(), &inverse, &CatalogGraphValidator)
        .unwrap();
    f.apply_prepared(&undo);
    assert_eq!(canonical_bytes(&f), after);
    let redo = f
        .engine
        .prepare_inverse(&f.writer(), &undo, &CatalogGraphValidator)
        .unwrap();
    let validator = InterruptReviewFilesApplied {
        accepted: a.clone(),
        tripped: AtomicBool::new(false),
        inverse: true,
    };
    assert!(
        f.engine
            .apply(&f.writer(), &redo, &validator, &f.catalog)
            .is_err()
    );
    assert!(validator.tripped.load(Ordering::SeqCst));
    assert_eq!(
        f.engine.inspect(&redo.change_id).unwrap().status,
        ChangeStatus::FilesApplied
    );
    let (p, n) = note(&f, &f.source);
    let bytes = lwiki::records::edit_note(
        &n,
        &BTreeMap::from([
            ("wiki_status".into(), json!("withdrawn")),
            (
                "wiki_withdrawal_reason".into(),
                json!("Withdrawn after inverse already restored accepted"),
            ),
            ("wiki_withdrawn_at".into(), json!("2026-09-28T00:00:00Z")),
        ]),
        None,
        &n.source_hash,
    )
    .unwrap();
    fs::write(f.root.path().join(p.as_str()), bytes).unwrap();
    assert!(
        f.engine
            .recover(&f.writer(), &CatalogGraphValidator, &f.catalog)
            .is_err()
    );
}
#[test]
fn canonical_review_reack_preserves_author_prose_and_later_decisions() {
    let f = resolved();
    let a = assertion(&f, "a1");
    let r = request(&f, &a, "accept", "supports");
    let accepted = review_apply(&f, &r);
    let v = validate_review(&f.view(), &serde_json::to_vec(&r).unwrap()).unwrap();
    let mut rejected = request(&f, &a, "reject", "contradicts");
    supersede(&f, &mut rejected, &accepted);
    let next = review_apply(&f, &rejected);
    edit(
        &f,
        &a,
        BTreeMap::from([("title".into(), json!("Author renamed reviewed claim"))]),
        Some(b"\nAuthor prose remains after historical acknowledgement.\n"),
    );
    let kept_before = canonical_bytes(&f);
    let retained_reack = stage_review(&f.engine, &f.writer(), &v).unwrap();
    assert_eq!(
        retained_reack.disposition,
        ReviewOutcomeDisposition::RetainedChange
    );
    assert_eq!(retained_reack.allocations, accepted.allocations);
    assert_eq!(kept_before, canonical_bytes(&f));
    fs::remove_dir_all(f.root.path().join("changes")).unwrap();
    let before = canonical_bytes(&f);
    let reused = stage_review(&f.engine, &f.writer(), &v).unwrap();
    assert_eq!(
        reused.disposition,
        ReviewOutcomeDisposition::CanonicalRestored
    );
    assert!(reused.prepared.is_none());
    assert_eq!(reused.allocations, accepted.allocations);
    assert_eq!(before, canonical_bytes(&f));
    assert_eq!(
        note(&f, &a).1.canonical.unwrap().string("wiki_status"),
        Some("rejected")
    );
    assert_eq!(
        note(&f, next.allocations.decisions.values().next().unwrap())
            .1
            .canonical
            .unwrap()
            .string("wiki_status"),
        Some("active")
    );
    assert!(load_extraction(&f.view(), &f.extraction).is_ok());
}
fn fixture_receipt(n: &lwiki::records::ParsedNote) -> ReviewReceiptV1 {
    let body = std::str::from_utf8(n.body()).unwrap();
    let json = body
        .split("```lwiki-graph-review-v1\n")
        .nth(1)
        .unwrap()
        .split("\n```")
        .next()
        .unwrap();
    serde_json::from_str(json).unwrap()
}
#[test]
fn unrelated_active_review_cannot_replace_explicit_history_chain() {
    let f = resolved();
    let a = assertion(&f, "a1");
    let first = review_apply(&f, &request(&f, &a, "accept", "supports"));
    let old_id = first.allocations.decisions[&a].clone();
    let (_, old_note) = note(&f, &old_id);
    let old_receipt = fixture_receipt(&old_note);
    let mut r = request(&f, &a, "accept", "supports");
    supersede(&f, &mut r, &first);
    let second = review_apply(&f, &r);
    let sid = second.allocations.decisions[&a].clone();
    let (sp, sn) = note(&f, &sid);
    let mut fake = fixture_receipt(&sn);
    let new_id = RecordId::generate(RecordKind::Decision).unwrap();
    fake.allocations.decisions.insert(a.clone(), new_id.clone());
    fake.assertion_proofs[0].governing_decision_id = new_id.clone();
    fake.record_paths.remove(&sid);
    fake.record_paths
        .insert(new_id.clone(), record_path("decisions", &new_id));
    fake.request.supersedes.clear();
    fake.predecessors.clear();
    fake.supersessions.clear();
    let generation = json!({
        "schema":"lwiki.graph-review-scope.v1",
        "assertions":[{
            "assertion_id":a,
            "expected_hash":fake.request.decisions[0].expected_hash,
            "evidence":fake.request.decisions[0].evidence_checks.iter().map(|e|
                json!({"evidence_id":e.evidence_id,"expected_hash":e.expected_hash})
            ).collect::<Vec<_>>(),
        }],
        "supersedes":[],
    });
    let hash = Blake3Hash::digest(lwiki::graph::packet::canonical_json(&generation).unwrap());
    fake.task_id = id(&format!(
        "review_{}",
        hash.as_str().trim_start_matches("blake3:")
    ));
    fake.request_hash =
        Blake3Hash::digest(lwiki::graph::packet::canonical_json(&fake.request).unwrap());
    let mut fields = sn.canonical.unwrap().fields().clone();
    fields.insert("wiki_id".into(), json!(new_id));
    fields.remove("wiki_supersedes_id");
    fields.remove("wiki_supersedes");
    let mut bytes = b"---\n".to_vec();
    for (k, v) in fields {
        bytes
            .extend_from_slice(format!("{k}: {}\n", serde_json::to_string(&v).unwrap()).as_bytes());
    }
    bytes.extend_from_slice(b"---\nUnrelated forged active review.\n\n```lwiki-graph-review-v1\n");
    bytes.extend(lwiki::graph::packet::canonical_json(&fake).unwrap());
    bytes.extend_from_slice(b"\n```\n");
    fs::remove_file(f.root.path().join(sp.as_str())).unwrap();
    fs::write(
        f.root
            .path()
            .join(record_path("decisions", &new_id).as_str()),
        bytes,
    )
    .unwrap();
    assert!(verify_review_evolution(&f.view(), &a, &old_receipt).is_err());
    assert!(load_review_receipt(&f.view(), &old_receipt.task_id).is_err());
}
#[test]
fn cleared_review_or_alias_origin_cannot_borrow_unsealed_receipt_authority() {
    for alias_case in [false, true] {
        let f = resolved();
        let out = if alias_case {
            stage(&f, &alias(&f, &bound_entity(&f, "m1")))
                .unwrap()
                .prepared
                .unwrap()
        } else {
            review_stage(&f, &request(&f, &assertion(&f, "a1"), "reject", "supports"))
                .unwrap()
                .prepared
                .unwrap()
        };
        let mut draft = proposed(&f, &out);
        fs::remove_dir_all(f.root.path().join(format!("changes/{}", out.change_id))).unwrap();
        draft.origin = None;
        let forged = f.engine.prepare(&f.writer(), draft).unwrap().prepared;
        let before = canonical_bytes(&f);
        assert!(
            f.engine
                .apply(&f.writer(), &forged, &CatalogGraphValidator, &f.catalog)
                .is_err()
        );
        assert_eq!(canonical_bytes(&f), before);
    }
}
#[test]
fn review_forward_admission_bounds_actual_files_before_publication() {
    let f = resolved();
    let a = assertion(&f, "a1");
    let out = review_stage(&f, &request(&f, &a, "accept", "supports")).unwrap();
    let before = note(&f, &a).1.raw;
    let dir = f.root.path().join("large-readable-notes");
    fs::create_dir_all(&dir).unwrap();
    for i in 0..4097 {
        fs::write(dir.join(format!("{i:05}.md")), b"plain author note\n").unwrap();
    }
    let err = f
        .engine
        .apply(
            &f.writer(),
            out.prepared.as_ref().unwrap(),
            &CatalogGraphValidator,
            &f.catalog,
        )
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::BudgetExceeded);
    assert_eq!(note(&f, &a).1.raw, before);
}
#[test]
fn review_apply_bounds_non_markdown_enumeration_before_publication() {
    let f = resolved();
    let a = assertion(&f, "a1");
    let out = review_stage(&f, &request(&f, &a, "accept", "supports")).unwrap();
    let before = canonical_bytes(&f);
    let base = f.root.path().join("many-non-markdown-entries");
    fs::create_dir(&base).unwrap();
    // Separate small directories exercise the real walk without a huge sibling
    // collision set. None of these entries can count toward the Markdown cap.
    for directory in 0..256 {
        let dir = base.join(format!("d{directory:03}"));
        fs::create_dir(&dir).unwrap();
        for entry in 0..256 {
            File::create(dir.join(format!("e{entry:03}.txt"))).unwrap();
        }
    }
    let err = f
        .engine
        .apply(
            &f.writer(),
            out.prepared.as_ref().unwrap(),
            &CatalogGraphValidator,
            &f.catalog,
        )
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::BudgetExceeded);
    // Read known canonical paths directly: another full enumeration is supposed
    // to be refused and is unnecessary to prove publication never began.
    for (path, bytes) in before {
        assert_eq!(fs::read(f.root.path().join(path)).unwrap(), bytes);
    }
    for decision in out.allocations.decisions.values() {
        assert!(
            !f.root
                .path()
                .join(record_path("decisions", decision).as_str())
                .exists()
        );
    }
}
#[test]
fn unchanged_accepted_reviews_use_complete_guard_generations() {
    let f = resolved();
    let a = assertion(&f, "a1");
    let first = review_apply(&f, &request(&f, &a, "accept", "supports"));
    let accepted_bytes = note(&f, &a).1.raw;
    let mut second_request = request(&f, &a, "accept", "supports");
    supersede(&f, &mut second_request, &first);
    let second = review_apply(&f, &second_request);
    assert_eq!(note(&f, &a).1.raw, accepted_bytes);

    let mut third_request = request(&f, &a, "accept", "supports");
    supersede(&f, &mut third_request, &second);
    let second_validated =
        validate_review(&f.view(), &serde_json::to_vec(&second_request).unwrap()).unwrap();
    let third_validated =
        validate_review(&f.view(), &serde_json::to_vec(&third_request).unwrap()).unwrap();
    assert_ne!(second_validated.task_id(), third_validated.task_id());
    let third = review_apply(&f, &third_request);
    assert_ne!(second.allocations, third.allocations);
    assert_eq!(note(&f, &a).1.raw, accepted_bytes);

    // Refresh and exact-quote revalidation change the evidence generation while
    // preserving the assertion bytes and the current governing Decision guard.
    let mut before_refresh = request(&f, &a, "accept", "supports");
    supersede(&f, &mut before_refresh, &third);
    let before_generation =
        validate_review(&f.view(), &serde_json::to_vec(&before_refresh).unwrap()).unwrap();
    let old_evidence = id(
        before_refresh["decisions"][0]["evidence_checks"][0]["evidence_id"]
            .as_str()
            .unwrap(),
    );
    let old_evidence_hash = note(&f, &old_evidence).1.source_hash;
    let store = SourceStore::new(f.engine.fs().clone());
    let refreshed = store
        .plan_refresh(
            &f.source,
            CaptureRequest {
                title: "Exact quotations retained in successor capture".into(),
                origin_kind: SourceOrigin::LocalFile,
                origin: "disposable.md".into(),
                original: format!(
                    "{}Additional author source line.\r\n",
                    include_str!("fixtures/p13/source.md").replace('\n', "\r\n")
                )
                .into_bytes(),
                extraction: ExtractionInput::Utf8Preserve,
                media_type: Some("text/markdown".into()),
            },
        )
        .unwrap();
    f.apply(refreshed.draft.unwrap());
    let revalidated = store
        .plan_revalidate(&old_evidence, &refreshed.revision_id, &old_evidence_hash)
        .unwrap();
    f.apply(revalidated.draft);
    let mut after_refresh = request(&f, &a, "accept", "supports");
    supersede(&f, &mut after_refresh, &third);
    assert_eq!(before_refresh["supersedes"], after_refresh["supersedes"]);
    assert_eq!(
        before_refresh["decisions"][0]["expected_hash"],
        after_refresh["decisions"][0]["expected_hash"]
    );
    let after_generation =
        validate_review(&f.view(), &serde_json::to_vec(&after_refresh).unwrap()).unwrap();
    assert_ne!(before_generation.task_id(), after_generation.task_id());
    let fourth = review_apply(&f, &after_refresh);
    assert_eq!(note(&f, &a).1.raw, accepted_bytes);
    assert_ne!(third.allocations, fourth.allocations);

    // Both alternate judgments are independently valid, but they cannot create
    // another result for a generation which already has a retained request.
    let mut original = request(&f, &a, "reject", "supports");
    supersede(&f, &mut original, &fourth);
    let validated = validate_review(&f.view(), &serde_json::to_vec(&original).unwrap()).unwrap();
    let staged = stage_review(&f.engine, &f.writer(), &validated).unwrap();
    let canonical_before = canonical_bytes(&f);
    for field in ["reason", "assessment", "decision"] {
        let mut alternate = original.clone();
        match field {
            "reason" => alternate["decisions"][0]["reason"] = json!("Different explicit rationale"),
            "assessment" => {
                alternate["decisions"][0]["evidence_checks"][0]["assessment"] =
                    json!("insufficient")
            }
            _ => alternate["decisions"][0]["decision"] = json!("accept"),
        }
        let competing =
            validate_review(&f.view(), &serde_json::to_vec(&alternate).unwrap()).unwrap();
        assert_eq!(validated.task_id(), competing.task_id());
        assert_ne!(validated.request_hash(), competing.request_hash());
        let err = stage_review(&f.engine, &f.writer(), &competing).unwrap_err();
        assert_eq!(err.code, ErrorCode::ContentConflict);
        assert_eq!(canonical_bytes(&f), canonical_before);
    }
    f.apply_prepared(staged.prepared.as_ref().unwrap());
}
#[test]
fn sparse_oversized_source_payload_after_stage_refuses_before_publication() {
    for payload_key in ["wiki_original_path", "wiki_content_path"] {
        let f = resolved();
        let a = assertion(&f, "a1");
        let req = request(&f, &a, "accept", "supports");
        let out = review_stage(&f, &req).unwrap();
        let evidence = id(req["decisions"][0]["evidence_checks"][0]["evidence_id"]
            .as_str()
            .unwrap());
        let (_, evidence_note) = note(&f, &evidence);
        let revision = id(evidence_note
            .canonical
            .unwrap()
            .string("wiki_source_revision")
            .unwrap());
        let (revision_path, revision_note) = note(&f, &revision);
        let payload = f
            .root
            .path()
            .join(revision_path.as_str())
            .parent()
            .unwrap()
            .join(
                revision_note
                    .canonical
                    .unwrap()
                    .string(payload_key)
                    .unwrap(),
            );
        // Native set_len creates a sparse disposable file; do not allocate or
        // read its bytes merely to set up or inspect this admission regression.
        fs::OpenOptions::new()
            .write(true)
            .open(&payload)
            .unwrap()
            .set_len(64 * 1024 * 1024 + 1)
            .unwrap();
        assert_eq!(fs::metadata(&payload).unwrap().len(), 64 * 1024 * 1024 + 1);
        let before = canonical_bytes(&f);
        let input = ValidationInput {
            vault_id: f.engine.vault_id().clone(),
            documents: before
                .iter()
                .map(|(path, bytes)| ScanDocument {
                    path: VaultRelativePath::new(path.clone()).unwrap(),
                    bytes: bytes.clone(),
                    hash: Blake3Hash::digest(bytes),
                })
                .collect(),
            overlay: vec![],
        };
        let projection_error = lwiki::catalog::scan::project(f.engine.fs(), &input).unwrap_err();
        assert_eq!(
            projection_error.code,
            ErrorCode::BudgetExceeded,
            "{payload_key}"
        );
        let err = f
            .engine
            .apply(
                &f.writer(),
                out.prepared.as_ref().unwrap(),
                &CatalogGraphValidator,
                &f.catalog,
            )
            .unwrap_err();
        // The engine's earlier read-precondition check records a durable conflict
        // when the bounded asset reread fails, before Catalog projection begins.
        assert_eq!(err.code, ErrorCode::ContentConflict, "{payload_key}");
        assert_eq!(canonical_bytes(&f), before, "{payload_key}");
    }
}
#[test]
fn review_inverse_sparse_source_bound_prevents_accepted_restoration_and_recovery() {
    for (payload_key, files_applied) in [
        ("wiki_original_path", false),
        ("wiki_content_path", false),
        ("wiki_content_path", true),
    ] {
        let f = resolved();
        let a = assertion(&f, "a1");
        let accept_request = request(&f, &a, "accept", "supports");
        let evidence = id(
            accept_request["decisions"][0]["evidence_checks"][0]["evidence_id"]
                .as_str()
                .unwrap(),
        );
        let (_, evidence_note) = note(&f, &evidence);
        let revision = id(evidence_note
            .canonical
            .unwrap()
            .string("wiki_source_revision")
            .unwrap());
        let (revision_path, revision_note) = note(&f, &revision);
        let payload = f
            .root
            .path()
            .join(revision_path.as_str())
            .parent()
            .unwrap()
            .join(
                revision_note
                    .canonical
                    .unwrap()
                    .string(payload_key)
                    .unwrap(),
            );
        let accepted = review_apply(&f, &accept_request);
        let mut reject = request(&f, &a, "reject", "contradicts");
        supersede(&f, &mut reject, &accepted);
        let rejected = review_apply(&f, &reject);
        assert_eq!(rejected.allocations.successors.len(), 1);
        let inverse = f
            .engine
            .prepare_inverse(
                &f.writer(),
                rejected.prepared.as_ref().unwrap(),
                &CatalogGraphValidator,
            )
            .unwrap();
        if files_applied {
            let validator = InterruptReviewFilesApplied {
                accepted: a.clone(),
                tripped: AtomicBool::new(false),
                inverse: true,
            };
            assert!(
                f.engine
                    .apply(&f.writer(), &inverse, &validator, &f.catalog)
                    .is_err()
            );
            assert!(validator.tripped.load(Ordering::SeqCst));
            assert_eq!(
                f.engine.inspect(&inverse.change_id).unwrap().status,
                ChangeStatus::FilesApplied
            );
            assert_eq!(
                note(&f, &a).1.canonical.unwrap().string("wiki_status"),
                Some("accepted")
            );
        }
        fs::OpenOptions::new()
            .write(true)
            .open(&payload)
            .unwrap()
            .set_len(64 * 1024 * 1024 + 1)
            .unwrap();
        let before = canonical_bytes(&f);
        let err = if files_applied {
            f.engine
                .recover(&f.writer(), &CatalogGraphValidator, &f.catalog)
                .unwrap_err()
        } else {
            f.engine
                .apply(&f.writer(), &inverse, &CatalogGraphValidator, &f.catalog)
                .unwrap_err()
        };
        assert_eq!(
            err.code,
            ErrorCode::BudgetExceeded,
            "{payload_key}, FilesApplied={files_applied}"
        );
        assert_eq!(canonical_bytes(&f), before);
        if files_applied {
            assert_ne!(
                f.engine.inspect(&inverse.change_id).unwrap().status,
                ChangeStatus::Committed
            );
        } else {
            assert_eq!(
                note(&f, &a).1.canonical.unwrap().string("wiki_status"),
                Some("rejected")
            );
        }
    }
}
#[test]
fn changed_stance_keeps_crlf_quotation_author_body_and_ordinary_null_metadata() {
    let f = resolved();
    let a = assertion(&f, "a1");
    let req = request(&f, &a, "reject", "contradicts");
    let eid = id(req["decisions"][0]["evidence_checks"][0]["evidence_id"]
        .as_str()
        .unwrap());
    let (ep, en) = note(&f, &eid);
    let r = en.canonical.as_ref().unwrap();
    let span = ByteSpan::new(
        r.field("wiki_span_start").unwrap().as_u64().unwrap(),
        r.field("wiki_span_end").unwrap().as_u64().unwrap(),
    )
    .unwrap();
    let citation = EvidenceRef {
        evidence_id: eid.clone(),
        assertion_id: a.clone(),
        source_id: id(r.string("wiki_source_id").unwrap()),
        source_revision: id(r.string("wiki_source_revision").unwrap()),
        span,
        quote_hash: Blake3Hash::new(r.string("wiki_quote_hash").unwrap()).unwrap(),
    };
    let quote = f
        .view()
        .verify(&CitationRef::Assertion(citation), CitationScope::Historical)
        .unwrap()
        .quote;
    let mut body = lwiki::sources::evidence::exact_quote_body(
        &quote,
        "\r\n",
        "Author café rationale outside the exact quote.",
    )
    .unwrap();
    body.extend_from_slice(b"\r\n<!-- Author comment retained. -->\r\n");
    let fields = r.fields().clone();
    let mut bytes = b"---\r\n".to_vec();
    for (k, v) in fields {
        bytes.extend_from_slice(
            format!("{k}: {}\r\n", serde_json::to_string(&v).unwrap()).as_bytes(),
        );
    }
    bytes.extend_from_slice(b"author_note: null\r\n---\r\n");
    bytes.extend_from_slice(&body);
    fs::write(f.root.path().join(ep.as_str()), bytes).unwrap();
    let outcome = review_apply(&f, &request(&f, &a, "reject", "contradicts"));
    let (_, successor) = note(&f, &outcome.allocations.successors[&eid]);
    assert_eq!(successor.body(), body);
    assert_eq!(
        successor.fields.unwrap().get("author_note"),
        Some(&Value::Null)
    );
    assert!(load_extraction(&f.view(), &f.extraction).is_ok());
}
#[test]
fn authentic_review_inverse_refuses_author_edits_extra_writes_and_missing_parent() {
    for variant in 0..3 {
        let f = resolved();
        let a = assertion(&f, "a1");
        let out = review_apply(&f, &request(&f, &a, "reject", "contradicts"));
        let original = out.prepared.as_ref().unwrap();
        if variant == 0 {
            edit(
                &f,
                &a,
                BTreeMap::from([("title".into(), json!("Preserved later author edit"))]),
                None,
            );
            let before = canonical_bytes(&f);
            assert!(
                f.engine
                    .prepare_inverse(&f.writer(), original, &CatalogGraphValidator)
                    .is_err()
            );
            assert_eq!(canonical_bytes(&f), before);
        } else if variant == 1 {
            let mut draft = f.engine.inverse_plan(original).unwrap().draft;
            let page = VaultRelativePath::new("pages/inverse-extra.md").unwrap();
            draft.operations.push(ExpectedWrite {
                target: page,
                expected: ExpectedState::Absent,
                proposed: Some(b"Unrequested inverse write\n".to_vec()),
                apply_after: vec![],
            });
            let forged = f.engine.prepare(&f.writer(), draft).unwrap().prepared;
            let before = canonical_bytes(&f);
            assert!(
                f.engine
                    .apply(&f.writer(), &forged, &CatalogGraphValidator, &f.catalog)
                    .is_err()
            );
            assert_eq!(canonical_bytes(&f), before);
        } else {
            let inverse = f
                .engine
                .prepare_inverse(&f.writer(), original, &CatalogGraphValidator)
                .unwrap();
            fs::remove_dir_all(
                f.root
                    .path()
                    .join(format!("changes/{}", original.change_id)),
            )
            .unwrap();
            let before = canonical_bytes(&f);
            assert!(
                f.engine
                    .apply(&f.writer(), &inverse, &CatalogGraphValidator, &f.catalog)
                    .is_err()
            );
            assert_eq!(canonical_bytes(&f), before);
        }
    }
}

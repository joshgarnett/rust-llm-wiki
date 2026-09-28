use lwiki::{
    catalog::*,
    changes::*,
    domain::*,
    graph::{
        decision_types::*, decisions::*, extraction_types::*, import::*, inverse::*, packet::*,
        resolution::*, resolution_types::*, wire::*,
    },
    records::{edit_note, parse_note},
    sources::*,
    vault::*,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path, time::Duration};
const VAULT: &str = "vault_00000000-0000-7000-8000-00000000001b";
fn id(s: &str) -> RecordId {
    RecordId::new(s).unwrap()
}
fn local(s: &str) -> PacketLocalId {
    PacketLocalId::new(s).unwrap()
}
fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy(&path, &target)
        } else {
            fs::copy(path, target).unwrap();
        }
    }
}
struct Fixture {
    _temp: tempfile::TempDir,
    root: VaultRoot,
    engine: ChangeEngine,
    catalog: Catalog,
    source: RecordId,
    extraction: RecordId,
    raw: Vec<u8>,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        copy(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bootstrap/vault"),
            temp.path(),
        );
        let root = VaultRoot::explicit(temp.path()).unwrap();
        let vault = VaultFs::new(root.clone());
        let engine = ChangeEngine::new(vault.clone()).unwrap();
        let catalog = Catalog::new(vault.clone(), id(VAULT));
        let text = include_str!("fixtures/p12/source.md").replace('\n', "\r\n");
        let capture = SourceStore::new(vault)
            .plan_capture(CaptureRequest {
                title: "Resolution source".into(),
                origin_kind: SourceOrigin::LocalFile,
                origin: "disposable.md".into(),
                original: text.as_bytes().to_vec(),
                extraction: ExtractionInput::Utf8Preserve,
                media_type: Some("text/markdown".into()),
            })
            .unwrap();
        let fixture = Self {
            _temp: temp,
            root,
            engine,
            catalog,
            source: capture.source_id.clone(),
            extraction: id("extraction_pending"),
            raw: vec![],
        };
        fixture.apply(capture.draft.unwrap());
        let request = ExportRequest {
            source_id: fixture.source.clone(),
            revision_id: Some(capture.revision_id),
            windows: vec![],
            limits: ExtractionLimits::default(),
            candidate_context: vec![],
        };
        let plan = build_packet(&fixture.view(), &request).unwrap();
        fixture.apply(plan.draft.unwrap());
        let packet = load_packet(&fixture.view(), &plan.packet.packet_id).unwrap();
        let mention = |id: &str, label: &str, kind: &str, start: usize| json!({"id":id,"window_id":"w1","label":label,"type":kind,"quote":label,"span":{"start":start,"end":start+label.len()}});
        let ada = text
            .match_indices("Ada")
            .map(|(n, _)| n)
            .collect::<Vec<_>>();
        let assertion = |id: &str, subject: &str, predicate: &str, object: Value, quote: &str| json!({"id":id,"subject":subject,"predicate":predicate,"object":object,"negated":false,"modality":"asserted","evidence":[{"window_id":"w1","stance":"supports","quote":quote}]});
        let mut literal = assertion(
            "a4",
            "m3",
            "has_property",
            json!({"kind":"literal","type":"string","value":"Ada"}),
            "Ada maintains Tool.",
        );
        literal["property"] = json!("name");
        let response = json!({"schema":EXTRACTION_SCHEMA,"packet_id":packet.packet().packet_id,"packet_fingerprint":packet.packet().packet_fingerprint,
            "mentions":[mention("m1","Ada","person",ada[0]),mention("m2","Acme","organization",text.find("Acme").unwrap()),mention("m3","Ada","person",ada[1]),mention("m4","She","person",text.find("She").unwrap()),mention("m5","Tool","component",text.find("Tool").unwrap())],
            "assertions":[assertion("a1","m1","works_for",json!({"kind":"mention","mention_id":"m2"}),"Ada works for Acme.\r\n"),assertion("a2","m3","maintains",json!({"kind":"mention","mention_id":"m5"}),"Ada maintains Tool."),assertion("a3","m4","uses",json!({"kind":"mention","mention_id":"m5"}),"She uses Tool."),literal],"unresolved":[]});
        let raw = format!(" \n{}\n ", serde_json::to_string(&response).unwrap()).into_bytes();
        let validated = validate_response(&packet, &fixture.view(), &raw).unwrap();
        let outcome = stage_import(
            &fixture.engine,
            &fixture.writer(),
            &validated,
            OriginPolicy::ReuseOrConflict,
        )
        .unwrap();
        fixture.apply_prepared(outcome.prepared.as_ref().unwrap());
        Self {
            extraction: outcome.extraction.record.unwrap().record_id,
            raw,
            ..fixture
        }
    }
    fn writer(&self) -> WriterPermit {
        WriterPermit::acquire(&self.root, Duration::from_secs(2)).unwrap()
    }
    fn view(&self) -> SourceView<'_> {
        SourceView::from_fs_bounded(self.engine.fs(), 64 * 1024 * 1024, 4096).unwrap()
    }
    fn apply(&self, draft: ChangeDraft) {
        let prepared = self.engine.prepare(&self.writer(), draft).unwrap().prepared;
        self.apply_prepared(&prepared);
    }
    fn apply_prepared(&self, prepared: &PreparedChange) {
        self.engine
            .apply(
                &self.writer(),
                prepared,
                &CatalogGraphValidator,
                &self.catalog,
            )
            .unwrap();
    }
    fn artifact(&self) -> ExtractionArtifactV1 {
        load_extraction(&self.view(), &self.extraction)
            .unwrap()
            .artifact()
            .clone()
    }
    fn request(&self, mappings: Vec<Value>) -> Value {
        let loaded = load_extraction(&self.view(), &self.extraction).unwrap();
        json!({"schema":RESOLUTION_SCHEMA,"extraction_id":self.extraction,"expected_hash":loaded.locator().observed_hash,"mappings":mappings})
    }
    fn validate(&self, request: &Value) -> ValidatedResolution {
        validate_resolution(&self.view(), &serde_json::to_vec(request).unwrap()).unwrap()
    }
    fn stage(&self, request: &Value) -> ResolutionOutcome {
        stage_resolution(&self.engine, &self.writer(), &self.validate(request)).unwrap()
    }
    fn resolve(&self, request: &Value) -> ResolutionOutcome {
        let outcome = self.stage(request);
        self.apply_prepared(outcome.prepared.as_ref().unwrap());
        outcome
    }
    fn existing(&self) -> RecordId {
        id("entity_00000000-0000-7000-8000-000000000001")
    }
    fn edit_status(&self, path: VaultRelativePath, status: &str) {
        let raw = fs::read(self.root.path().join(path.as_str())).unwrap();
        let note = parse_note(&raw);
        let proposed = edit_note(
            &note,
            &BTreeMap::from([("wiki_status".into(), json!(status))]),
            None,
            &note.source_hash,
        )
        .unwrap();
        self.apply(ChangeDraft {
            title: "Explicit fixture review".into(),
            origin: None,
            inverse_of: None,
            allocated_ids: BTreeMap::new(),
            read_preconditions: vec![],
            operations: vec![ExpectedWrite {
                target: path,
                expected: ExpectedState::Hash(note.source_hash),
                proposed: Some(proposed),
                apply_after: vec![],
            }],
        });
    }
}
fn create(mention: &str, title: &str, kind: &str) -> Value {
    json!({"operation":"CreateEntity","mention_id":mention,"reason":"Explicit distinct identity","title":title,"entity_type":kind})
}
fn creates() -> Vec<Value> {
    vec![
        create("m1", "Ada", "person"),
        create("m2", "Acme", "organization"),
        create("m3", "Ada", "person"),
        create("m4", "She", "person"),
        create("m5", "Tool", "component"),
    ]
}
fn record_path(kind: &str, id: &RecordId) -> VaultRelativePath {
    VaultRelativePath::new(format!("knowledge/{kind}/{id}.md")).unwrap()
}

fn resolved() -> Fixture {
    let f = Fixture::new();
    f.resolve(&f.request(creates()));
    f
}
fn bound_entity(f: &Fixture, mention: &str) -> RecordId {
    match &f.artifact().bindings[&local(mention)] {
        MentionBinding::Resolved { entity_id, .. } => entity_id.clone(),
        _ => panic!("resolved fixture endpoint"),
    }
}
fn guards(f: &Fixture) -> Value {
    json!(
        f.root
            .scan_markdown()
            .unwrap()
            .into_iter()
            .filter_map(|p| {
                let n = parse_note(&fs::read(f.root.path().join(p.as_str())).unwrap());
                n.canonical
                    .map(|r| json!({"record_id":r.id(),"hash":n.source_hash}))
            })
            .collect::<Vec<_>>()
    )
}
fn refs(f: &Fixture, entity: &RecordId, target: Value) -> Vec<Value> {
    let mut refs = vec![];
    for path in f.root.scan_markdown().unwrap() {
        let n = parse_note(&fs::read(f.root.path().join(path.as_str())).unwrap());
        if let Some(r) = &n.canonical
            && r.kind() == RecordKind::Assertion
        {
            for field in ["subject_id", "object_id"] {
                if r.string(&format!("wiki_{field}")) == Some(entity.as_str()) {
                    refs.push(json!({"kind":"Assertion","assertion_id":r.id(),"field":field,"old_entity_id":entity,"target":target}));
                }
            }
        }
    }
    for (mention, binding) in f.artifact().bindings {
        if matches!(binding,MentionBinding::Resolved{entity_id,..} if &entity_id==entity) {
            refs.push(json!({"kind":"Mention","extraction_id":f.extraction,"mention_id":mention,"old_entity_id":entity,"target":target}));
        }
    }
    refs
}
fn merge(f: &Fixture, source: &RecordId, target: &RecordId) -> Value {
    json!({"schema":ENTITY_DECISIONS_SCHEMA,"decisions":[{"operation":"MergeEntities","source_ids":[source],"target_id":target,"reason":"Explicit fixture identity merge","expected_records":guards(f),"remaps":refs(f,source,json!({"kind":"ExistingEntity","entity_id":target}))}]})
}
fn split(f: &Fixture, source: &RecordId) -> Value {
    json!({"schema":ENTITY_DECISIONS_SCHEMA,"decisions":[{"operation":"SplitEntity","source_id":source,"reason":"Explicit exhaustive identity partition","expected_records":guards(f),"new_entities":[{"key":"left","title":"Ada left","entity_type":"person"},{"key":"right","title":"Ada right","entity_type":"person"}],"remaps":refs(f,source,json!({"kind":"NewEntity","key":"left"}))}]})
}
fn alias(f: &Fixture, entity: &RecordId) -> Value {
    json!({"schema":ENTITY_DECISIONS_SCHEMA,"decisions":[{"operation":"AddAlias","entity_id":entity,"alias":"Explicit Ada alias","reason":"Explicit label only","expected_records":guards(f),"remaps":[]}]})
}
fn stage(f: &Fixture, request: &Value) -> Result<EntityDecisionOutcome> {
    let v = validate_entity_decisions(&f.view(), &serde_json::to_vec(request).unwrap())?;
    stage_entity_decisions(&f.engine, &f.writer(), &v)
}
fn decide(f: &Fixture, request: &Value) -> EntityDecisionOutcome {
    let result = stage(f, request).unwrap();
    f.engine
        .apply(
            &f.writer(),
            result.prepared.as_ref().unwrap(),
            &CatalogGraphValidator,
            &f.catalog,
        )
        .unwrap();
    result
}
fn canonical_bytes(f: &Fixture) -> BTreeMap<String, Vec<u8>> {
    f.root
        .scan_markdown()
        .unwrap()
        .into_iter()
        .map(|p| {
            (
                p.as_str().to_owned(),
                fs::read(f.root.path().join(p.as_str())).unwrap(),
            )
        })
        .collect()
}
fn assertion(f: &Fixture, local_id: &str) -> RecordId {
    f.artifact().allocations.assertions[&local(local_id)].clone()
}
fn note(f: &Fixture, id: &RecordId) -> (VaultRelativePath, lwiki::records::ParsedNote) {
    f.root
        .scan_markdown()
        .unwrap()
        .into_iter()
        .find_map(|p| {
            let n = parse_note(&fs::read(f.root.path().join(p.as_str())).unwrap());
            n.canonical
                .as_ref()
                .is_some_and(|r| r.id() == id)
                .then_some((p, n))
        })
        .unwrap()
}

fn authored(record: CanonicalRecord, body: &str) -> Vec<u8> {
    let mut text = String::from("---\n");
    for (key, value) in record.fields() {
        text.push_str(key);
        text.push_str(": ");
        text.push_str(&serde_json::to_string(value).unwrap());
        text.push('\n');
    }
    text.push_str("---\n");
    text.push_str(body);
    text.into_bytes()
}
fn accept_decision(f: &Fixture, assertions: &[RecordId]) -> RecordId {
    let id = RecordId::generate(RecordKind::Decision).unwrap();
    let fields = BTreeMap::from([
        ("wiki_schema".into(), json!("1")),
        ("wiki_id".into(), json!(id)),
        ("wiki_kind".into(), json!("decision")),
        ("title".into(), json!("Explicit fixture acceptance")),
        ("wiki_status".into(), json!("active")),
        ("wiki_action".into(), json!("accept")),
        ("wiki_created_at".into(), json!("2026-09-28T00:00:00Z")),
        ("wiki_input_ids".into(), json!(assertions)),
        ("wiki_output_ids".into(), json!(assertions)),
    ]);
    f.apply(ChangeDraft {
        title: "Explicit acceptance authority fixture".into(),
        origin: None,
        inverse_of: None,
        allocated_ids: BTreeMap::new(),
        read_preconditions: vec![],
        operations: vec![ExpectedWrite {
            target: record_path("decisions", &id),
            expected: ExpectedState::Absent,
            proposed: Some(authored(
                CanonicalRecord::new(fields).unwrap(),
                "Explicit prior review.\n",
            )),
            apply_after: vec![],
        }],
    });
    id
}

fn reverse(f: &Fixture, parent: &PreparedChange) -> PreparedChange {
    let inverse = f
        .engine
        .prepare_inverse(&f.writer(), parent, &CatalogGraphValidator)
        .unwrap();
    f.apply_prepared(&inverse);
    inverse
}
fn immutable_capture(f: &Fixture) -> BTreeMap<String, Vec<u8>> {
    fn walk(path: &Path, root: &Path, files: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, root, files);
            } else {
                files.insert(
                    path.strip_prefix(root).unwrap().to_str().unwrap().into(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    walk(
        &f.root
            .path()
            .join("sources")
            .join(f.source.as_str())
            .join("revisions"),
        f.root.path(),
        &mut files,
    );
    files
}
fn withdrawn_draft(f: &Fixture) -> ChangeDraft {
    SourceStore::new(f.engine.fs().clone())
        .plan_withdraw(&f.source, "Explicit fixture withdrawal of current support")
        .unwrap()
        .draft
        .unwrap()
}
fn external_withdraw(f: &Fixture, draft: ChangeDraft) {
    for op in draft.operations {
        fs::write(f.root.path().join(op.target.as_str()), op.proposed.unwrap()).unwrap();
    }
}
fn accepted_merge(f: &Fixture) -> (PreparedChange, RecordId) {
    let assertion = assertion(f, "a1");
    f.edit_status(note(f, &assertion).0, "accepted");
    accept_decision(f, std::slice::from_ref(&assertion));
    let source = bound_entity(f, "m1");
    let target = bound_entity(f, "m3");
    let out = decide(f, &merge(f, &source, &target));
    (out.prepared.unwrap(), assertion)
}

#[test]
fn merge_and_split_inverse_restore_exact_bytes_and_undo_inverse() {
    for partition in [false, true] {
        let f = resolved();
        let source = bound_entity(&f, "m1");
        let before = canonical_bytes(&f);
        let captured = immutable_capture(&f);
        let out = decide(
            &f,
            &if partition {
                split(&f, &source)
            } else {
                merge(&f, &source, &f.existing())
            },
        );
        let after = canonical_bytes(&f);
        let inverse = reverse(&f, out.prepared.as_ref().unwrap());
        assert_eq!(canonical_bytes(&f), before);
        assert_eq!(bound_entity(&f, "m1"), source);
        let inverse2 = reverse(&f, &inverse);
        assert_eq!(canonical_bytes(&f), after);
        reverse(&f, &inverse2);
        assert_eq!(canonical_bytes(&f), before);
        assert_eq!(immutable_capture(&f), captured);
    }
}

#[test]
fn committed_anchor_skips_obsolete_unmodified_guards_and_preserves_source_lifecycle() {
    let f = resolved();
    let source = bound_entity(&f, "m1");
    let out = decide(&f, &merge(&f, &source, &bound_entity(&f, "m3")));
    let capture = immutable_capture(&f);
    f.apply(withdrawn_draft(&f));
    let source_bytes = note(&f, &f.source).1.raw;
    reverse(&f, out.prepared.as_ref().unwrap());
    assert_eq!(note(&f, &f.source).1.raw, source_bytes);
    assert_eq!(immutable_capture(&f), capture);
    assert_eq!(bound_entity(&f, "m1"), source);
    assert_ne!(
        f.catalog.index_snapshot().unwrap().projection().records[&assertion(&f, "a1")].eligibility,
        Eligibility::Current
    );
}

#[test]
fn alias_noop_inverse_uses_committed_proof_after_target_superseded() {
    let f = resolved();
    let source = bound_entity(&f, "m1");
    decide(&f, &alias(&f, &source));
    let second = decide(&f, &alias(&f, &source));
    let manifest = f
        .engine
        .inspect(&second.prepared.as_ref().unwrap().change_id)
        .unwrap()
        .manifest;
    assert_eq!(
        manifest.operations.len(),
        1,
        "same alias creates only explicit Decision"
    );
    decide(&f, &merge(&f, &source, &bound_entity(&f, "m3")));
    let entity = note(&f, &source).1.raw;
    reverse(&f, second.prepared.as_ref().unwrap());
    assert_eq!(note(&f, &source).1.raw, entity);
    assert_eq!(
        note(&f, &source).1.canonical.unwrap().string("wiki_status"),
        Some("superseded")
    );
}

#[test]
fn split_inverse_refuses_added_assertion_and_resolved_mention_references() {
    for mention in [false, true] {
        let f = resolved();
        let source = bound_entity(&f, "m1");
        let out = decide(&f, &split(&f, &source));
        let created = out.allocations[0].entities[&NewEntityKey::new("left").unwrap()].clone();
        if mention {
            additional_resolved_extraction(&f, &created);
        } else {
            let extra = id("assertion_added_after_split");
            let mut fields = note(&f, &assertion(&f, "a1"))
                .1
                .canonical
                .unwrap()
                .fields()
                .clone();
            fields.insert("wiki_id".into(), json!(extra));
            f.apply(ChangeDraft {
                title: "Explicit additional split identity reference".into(),
                origin: None,
                inverse_of: None,
                allocated_ids: BTreeMap::new(),
                read_preconditions: vec![],
                operations: vec![ExpectedWrite {
                    target: record_path("assertions", &extra),
                    expected: ExpectedState::Absent,
                    proposed: Some(authored(
                        CanonicalRecord::new(fields).unwrap(),
                        "Author explicitly refers to split identity.\n",
                    )),
                    apply_after: vec![],
                }],
            });
        }
        let before = canonical_bytes(&f);
        assert!(
            f.engine
                .prepare_inverse(
                    &f.writer(),
                    out.prepared.as_ref().unwrap(),
                    &CatalogGraphValidator
                )
                .is_err()
        );
        assert_eq!(canonical_bytes(&f), before);
    }
}

#[test]
fn inverse_preserves_author_edits_and_rejects_missing_corrupt_noncommitted_parent() {
    for defect in [
        "author",
        "missing",
        "corrupt",
        "noncommitted",
        "missing_parent",
    ] {
        let f = resolved();
        let source = bound_entity(&f, "m1");
        let out = if defect == "noncommitted" {
            stage(&f, &merge(&f, &source, &bound_entity(&f, "m3"))).unwrap()
        } else {
            decide(&f, &merge(&f, &source, &bound_entity(&f, "m3")))
        };
        let parent = out.prepared.as_ref().unwrap();
        let manifest = f.engine.inspect(&parent.change_id).unwrap().manifest;
        match defect {
            "author" => {
                let (path, n) = note(&f, &source);
                f.apply(ChangeDraft {
                    title: "Preserve later author entity text".into(),
                    origin: None,
                    inverse_of: None,
                    allocated_ids: BTreeMap::new(),
                    read_preconditions: vec![],
                    operations: vec![ExpectedWrite {
                        target: path,
                        expected: ExpectedState::Hash(n.source_hash.clone()),
                        proposed: Some(
                            edit_note(
                                &n,
                                &BTreeMap::from([("title".into(), json!("Author revised title"))]),
                                Some(b"Author revised prose.\n"),
                                &n.source_hash,
                            )
                            .unwrap(),
                        ),
                        apply_after: vec![],
                    }],
                });
            }
            "missing" => {
                let payload = manifest
                    .operations
                    .iter()
                    .find_map(|op| op.before_payload.as_ref())
                    .unwrap();
                fs::remove_file(f.root.path().join(payload.path.as_str())).unwrap();
            }
            "corrupt" => {
                let payload = manifest
                    .operations
                    .iter()
                    .find_map(|op| op.after_payload.as_ref())
                    .unwrap();
                fs::write(
                    f.root.path().join(payload.path.as_str()),
                    b"forged original proposed bytes",
                )
                .unwrap();
            }
            "noncommitted" => {
                let app =
                    lwiki::app::OfflineApp::new(f.engine.fs().clone(), Default::default()).unwrap();
                for (i, op) in manifest.operations.iter().enumerate() {
                    let bytes = app
                        .changes_payload(parent.change_id.clone(), i)
                        .unwrap()
                        .proposed
                        .unwrap();
                    fs::write(f.root.path().join(op.target.as_str()), bytes).unwrap();
                }
                let path = lwiki::changes::prepare::manifest_path(&parent.change_id).unwrap();
                let note = parse_note(&fs::read(f.root.path().join(path.as_str())).unwrap());
                let bytes = edit_note(
                    &note,
                    &BTreeMap::from([("wiki_status".into(), json!("committed"))]),
                    None,
                    &note.source_hash,
                )
                .unwrap();
                fs::write(f.root.path().join(path.as_str()), bytes).unwrap();
                assert_eq!(
                    f.engine.inspect(&parent.change_id).unwrap().note_status,
                    "committed"
                );
            }
            "missing_parent" => {
                fs::remove_dir_all(
                    f.root
                        .path()
                        .join("changes")
                        .join(parent.change_id.as_str()),
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        let before = canonical_bytes(&f);
        assert!(
            f.engine
                .prepare_inverse(&f.writer(), parent, &CatalogGraphValidator)
                .is_err(),
            "{defect}"
        );
        assert_eq!(canonical_bytes(&f), before, "{defect}");
    }
}

#[test]
fn engine_apply_refuses_extra_inverse_write_and_forged_lineage() {
    for defect in ["extra", "unknown", "origin", "allocation", "wrong_proposed"] {
        let f = resolved();
        let source = bound_entity(&f, "m1");
        let out = decide(&f, &merge(&f, &source, &bound_entity(&f, "m3")));
        let mut draft = f
            .engine
            .inverse_plan(out.prepared.as_ref().unwrap())
            .unwrap()
            .draft;
        match defect {
            "extra" => draft.operations.push(ExpectedWrite {
                target: VaultRelativePath::new("pages/hidden_inverse.md").unwrap(),
                expected: ExpectedState::Absent,
                proposed: Some(b"Unlisted inverse write\n".to_vec()),
                apply_after: vec![],
            }),
            "unknown" => draft.inverse_of = Some(id("change_uncommitted_unknown")),
            "origin" => {
                draft.origin = Some(ChangeOrigin {
                    operation: OriginOperation::GraphDecide,
                    packet_id: id("entity_decisions_forged_inverse"),
                    response_hash: Blake3Hash::digest(b"forged inverse origin"),
                })
            }
            "allocation" => {
                draft
                    .allocated_ids
                    .insert("unlisted".into(), id("entity_unlisted_inverse"));
            }
            "wrong_proposed" => {
                let op = draft
                    .operations
                    .iter_mut()
                    .find(|op| op.proposed.is_some())
                    .unwrap();
                op.proposed
                    .as_mut()
                    .unwrap()
                    .extend_from_slice(b"Forged extra author prose.\n");
            }
            _ => unreachable!(),
        }
        let forged = f.engine.prepare(&f.writer(), draft).unwrap();
        let before = canonical_bytes(&f);
        assert!(
            f.engine
                .apply(
                    &f.writer(),
                    &forged.prepared,
                    &CatalogGraphValidator,
                    &f.catalog
                )
                .is_err(),
            "{defect}"
        );
        assert_eq!(canonical_bytes(&f), before, "{defect}");
    }
}

fn additional_resolved_extraction(f: &Fixture, entity: &RecordId) {
    let request = ExportRequest {
        source_id: f.source.clone(),
        revision_id: None,
        windows: vec![],
        limits: ExtractionLimits {
            max_mentions: 63,
            ..Default::default()
        },
        candidate_context: vec![],
    };
    let plan = build_packet(&f.view(), &request).unwrap();
    f.apply(plan.draft.unwrap());
    let packet = load_packet(&f.view(), &plan.packet.packet_id).unwrap();
    let mut response: Value = serde_json::from_slice(&f.raw).unwrap();
    response["packet_id"] = json!(packet.packet().packet_id);
    response["packet_fingerprint"] = json!(packet.packet().packet_fingerprint);
    response["mentions"] = json!([response["mentions"][0].clone()]);
    response["assertions"] = json!([]);
    let raw = serde_json::to_vec(&response).unwrap();
    let validated = validate_response(&packet, &f.view(), &raw).unwrap();
    let imported = stage_import(
        &f.engine,
        &f.writer(),
        &validated,
        OriginPolicy::ReuseOrConflict,
    )
    .unwrap();
    f.apply_prepared(imported.prepared.as_ref().unwrap());
    let extraction = imported.extraction.record.unwrap().record_id;
    let loaded = load_extraction(&f.view(), &extraction).unwrap();
    let request = json!({"schema":RESOLUTION_SCHEMA,"extraction_id":extraction,
        "expected_hash":loaded.locator().observed_hash,"mappings":[{"operation":"BindMention",
        "mention_id":"m1","reason":"Explicit independently retained mention binding",
        "entity_id":entity,"expected_entity_hash":note(f,entity).1.source_hash}]});
    let validated = validate_resolution(&f.view(), &serde_json::to_vec(&request).unwrap()).unwrap();
    let out = stage_resolution(&f.engine, &f.writer(), &validated).unwrap();
    f.apply_prepared(out.prepared.as_ref().unwrap());
}

use std::{
    fs::File,
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
struct InterruptInverseIo {
    after_decision_remove: bool,
    tripped: AtomicBool,
}
impl DurableIo for InterruptInverseIo {
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
        if !self.after_decision_remove
            && t.parent()
                .is_some_and(|p| p.ends_with("knowledge/assertions"))
            && !self.tripped.swap(true, Ordering::SeqCst)
        {
            return Err(io::Error::other(
                "real inverse post-assertion replacement interruption",
            ));
        }
        Ok(())
    }
    fn remove(&self, p: &Path) -> io::Result<()> {
        NativeIo.remove(p)?;
        if self.after_decision_remove
            && p.parent()
                .is_some_and(|p| p.ends_with("knowledge/decisions"))
            && !self.tripped.swap(true, Ordering::SeqCst)
        {
            return Err(io::Error::other(
                "real inverse post-receipt deletion interruption",
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
fn partial_inverse_recovers_without_current_forward_receipt_and_refuses_new_refs() {
    for (after_remove, extra_reference) in [(false, false), (true, false), (true, true)] {
        let f = resolved();
        let source = bound_entity(&f, "m1");
        let before = canonical_bytes(&f);
        let captured = immutable_capture(&f);
        let out = decide(&f, &split(&f, &source));
        let original_after_assertion = note(&f, &assertion(&f, "a1")).1.canonical.unwrap();
        let inverse = f
            .engine
            .prepare_inverse(
                &f.writer(),
                out.prepared.as_ref().unwrap(),
                &CatalogGraphValidator,
            )
            .unwrap();
        let io = Arc::new(InterruptInverseIo {
            after_decision_remove: after_remove,
            tripped: AtomicBool::new(false),
        });
        let interrupted = ChangeEngine::new(VaultFs::with_io(f.root.clone(), io.clone())).unwrap();
        assert!(
            interrupted
                .apply(&f.writer(), &inverse, &CatalogGraphValidator, &f.catalog)
                .is_err()
        );
        assert!(io.tripped.load(Ordering::SeqCst));
        if after_remove {
            assert!(
                !f.root
                    .path()
                    .join(record_path("decisions", &out.allocations[0].decision_id).as_str())
                    .exists()
            );
        }
        if extra_reference {
            let extra = id("assertion_author_added_during_inverse");
            let mut fields = original_after_assertion.fields().clone();
            fields.insert("wiki_id".into(), json!(extra));
            fs::write(
                f.root
                    .path()
                    .join(record_path("assertions", &extra).as_str()),
                authored(
                    CanonicalRecord::new(fields).unwrap(),
                    "New independent author reference during inverse.\n",
                ),
            )
            .unwrap();
            let current = canonical_bytes(&f);
            assert!(
                f.engine
                    .recover(&f.writer(), &CatalogGraphValidator, &f.catalog)
                    .is_err()
            );
            assert_eq!(canonical_bytes(&f), current);
        } else {
            let report = f
                .engine
                .recover(&f.writer(), &CatalogGraphValidator, &f.catalog)
                .unwrap();
            assert!(
                report
                    .changes
                    .iter()
                    .any(|c| c.change == inverse && c.status == ChangeStatus::Committed)
            );
            assert_eq!(canonical_bytes(&f), before);
        }
        assert_eq!(immutable_capture(&f), captured);
    }
}

struct InterruptFilesApplied {
    accepted: RecordId,
    tripped: AtomicBool,
    observed_accepted: AtomicBool,
}
impl GraphValidator for InterruptFilesApplied {
    fn validate(&self, fs: &VaultFs, input: &ValidationInput) -> Result<ValidatedGraph> {
        CatalogGraphValidator.validate(fs, input)
    }
    fn validate_retained(
        &self,
        fs: &VaultFs,
        input: &ValidationInput,
        w: &RetainedGraphInput,
    ) -> Result<ValidatedGraph> {
        CatalogGraphValidator.validate_retained(fs, input, w)
    }
    fn validate_inverse(
        &self,
        fs: &VaultFs,
        input: &ValidationInput,
        w: &RetainedGraphInverseInput,
    ) -> Result<ValidatedGraph> {
        let proof = verify_inverse_overlay(fs, input, w)?;
        assert!(proof.restored_accepted().contains(&self.accepted));
        let already = input.documents.iter().any(|d| {
            parse_note(&d.bytes).canonical.as_ref().is_some_and(|r| {
                r.id() == &self.accepted && r.string("wiki_status") == Some("accepted")
            })
        });
        if already {
            self.observed_accepted.store(true, Ordering::SeqCst);
        }
        let graph = CatalogGraphValidator.validate_inverse(fs, input, w)?;
        let reversed = w
            .parent_operations()
            .iter()
            .filter(|op| op.role() != OperationRole::ImmutableAsset)
            .all(|op| {
                let current = input
                    .documents
                    .iter()
                    .find(|d| &d.path == op.path())
                    .map_or(ExpectedState::Absent, |d| {
                        ExpectedState::Hash(d.hash.clone())
                    });
                current == *op.before()
            });
        if reversed && !self.tripped.swap(true, Ordering::SeqCst) {
            return Err(WikiError::new(
                ErrorCode::Internal,
                "real interruption after inverse FilesApplied validation",
            ));
        }
        Ok(graph)
    }
}

#[test]
fn accepted_inverse_restoration_requires_current_support_before_any_write() {
    for staged_before_withdraw in [false, true] {
        let f = resolved();
        let (parent, assertion) = accepted_merge(&f);
        let prepared = if staged_before_withdraw {
            Some(
                f.engine
                    .prepare_inverse(&f.writer(), &parent, &CatalogGraphValidator)
                    .unwrap(),
            )
        } else {
            None
        };
        f.apply(withdrawn_draft(&f));
        let before = canonical_bytes(&f);
        if let Some(inverse) = prepared {
            assert!(
                f.engine
                    .apply(&f.writer(), &inverse, &CatalogGraphValidator, &f.catalog)
                    .is_err()
            );
        } else {
            assert!(
                f.engine
                    .prepare_inverse(&f.writer(), &parent, &CatalogGraphValidator)
                    .is_err()
            );
        }
        assert_eq!(canonical_bytes(&f), before);
        assert_eq!(
            note(&f, &assertion)
                .1
                .canonical
                .unwrap()
                .string("wiki_status"),
            Some("proposed")
        );
    }
}

#[test]
fn files_applied_inverse_recovery_rechecks_already_restored_acceptance() {
    for withdraw in [false, true] {
        let f = resolved();
        let (parent, assertion) = accepted_merge(&f);
        let withdrawal = withdrawn_draft(&f);
        let inverse = f
            .engine
            .prepare_inverse(&f.writer(), &parent, &CatalogGraphValidator)
            .unwrap();
        let validator = InterruptFilesApplied {
            accepted: assertion.clone(),
            tripped: AtomicBool::new(false),
            observed_accepted: AtomicBool::new(false),
        };
        assert!(
            f.engine
                .apply(&f.writer(), &inverse, &validator, &f.catalog)
                .is_err()
        );
        assert!(validator.tripped.load(Ordering::SeqCst));
        assert!(validator.observed_accepted.load(Ordering::SeqCst));
        assert_eq!(
            f.engine.inspect(&inverse.change_id).unwrap().status,
            ChangeStatus::FilesApplied
        );
        assert_eq!(
            note(&f, &assertion)
                .1
                .canonical
                .unwrap()
                .string("wiki_status"),
            Some("accepted")
        );
        if withdraw {
            external_withdraw(&f, withdrawal);
            let before = canonical_bytes(&f);
            assert!(
                f.engine
                    .recover(&f.writer(), &CatalogGraphValidator, &f.catalog)
                    .is_err()
            );
            assert_eq!(canonical_bytes(&f), before);
            assert_ne!(
                f.catalog.index_snapshot().unwrap().projection().records[&assertion].eligibility,
                Eligibility::Current
            );
        } else {
            f.engine
                .recover(&f.writer(), &CatalogGraphValidator, &f.catalog)
                .unwrap();
            assert_eq!(
                f.catalog.index_snapshot().unwrap().projection().records[&assertion].eligibility,
                Eligibility::Current
            );
        }
    }
}

#[test]
fn applying_inverse_recovery_rechecks_partially_restored_acceptance() {
    let f = resolved();
    let (parent, assertion) = accepted_merge(&f);
    let withdrawal = withdrawn_draft(&f);
    let inverse = f
        .engine
        .prepare_inverse(&f.writer(), &parent, &CatalogGraphValidator)
        .unwrap();
    let io = Arc::new(InterruptInverseIo {
        after_decision_remove: false,
        tripped: AtomicBool::new(false),
    });
    let interrupted = ChangeEngine::new(VaultFs::with_io(f.root.clone(), io.clone())).unwrap();
    assert!(
        interrupted
            .apply(&f.writer(), &inverse, &CatalogGraphValidator, &f.catalog)
            .is_err()
    );
    assert!(io.tripped.load(Ordering::SeqCst));
    assert_eq!(
        note(&f, &assertion)
            .1
            .canonical
            .unwrap()
            .string("wiki_status"),
        Some("accepted")
    );
    external_withdraw(&f, withdrawal);
    let before = canonical_bytes(&f);
    assert!(
        f.engine
            .recover(&f.writer(), &CatalogGraphValidator, &f.catalog)
            .is_err()
    );
    assert_eq!(canonical_bytes(&f), before);
}

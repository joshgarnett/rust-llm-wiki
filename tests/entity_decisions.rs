use lwiki::{
    catalog::*,
    changes::*,
    domain::*,
    graph::{
        decision_types::*, decisions::*, extraction_types::*, import::*, packet::*, remap::*,
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
    fn bind(&self, local: &str, entity: &RecordId) -> Value {
        let (_, parsed) = self.entity_note(entity);
        json!({"operation":"BindMention","mention_id":local,"reason":"Explicit fixture identity","entity_id":entity,"expected_entity_hash":parsed.source_hash})
    }
    fn entity_note(&self, entity: &RecordId) -> (VaultRelativePath, lwiki::records::ParsedNote) {
        for path in self.root.scan_markdown().unwrap() {
            let note = parse_note(&fs::read(self.root.path().join(path.as_str())).unwrap());
            if note.canonical.as_ref().is_some_and(|r| r.id() == entity) {
                return (path, note);
            }
        }
        panic!("missing fixture entity {entity}");
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

#[test]
fn merge_requires_complete_hashed_remaps() {
    let f = resolved();
    let source = bound_entity(&f, "m1");
    let target = bound_entity(&f, "m3");
    let request = merge(&f, &source, &target);
    let before = canonical_bytes(&f);
    let mut incomplete = request.clone();
    incomplete["decisions"][0]["remaps"]
        .as_array_mut()
        .unwrap()
        .pop();
    assert!(stage(&f, &incomplete).is_err());
    assert_eq!(canonical_bytes(&f), before);
    let mut missing = request.clone();
    missing["decisions"][0]["expected_records"]
        .as_array_mut()
        .unwrap()
        .retain(|v| v["record_id"] != json!(source));
    assert!(stage(&f, &missing).is_err());
    let raw = f.artifact().raw_response.clone();
    let allocation = f.artifact().allocations.clone();
    let result = decide(&f, &request);
    let current = f.artifact();
    assert_eq!(current.raw_response, raw);
    assert_eq!(current.allocations, allocation);
    assert_eq!(bound_entity(&f, "m1"), target);
    assert_eq!(
        note(&f, &source).1.canonical.unwrap().string("wiki_status"),
        Some("superseded")
    );
    let a = note(&f, &assertion(&f, "a1")).1.canonical.unwrap();
    assert_eq!(a.string("wiki_subject_id"), Some(target.as_str()));
    assert_eq!(a.string("wiki_status"), Some("proposed"));
    assert_eq!(result.allocations[0].mention_decisions.len(), 1);
    // Independent unchanged target-side P11 authority remains valid.
    assert!(load_extraction(&f.view(), &f.extraction).is_ok());
}
#[test]
fn split_exhaustive_partition_or_conflict() {
    let f = resolved();
    let source = bound_entity(&f, "m1");
    let request = split(&f, &source);
    let before = canonical_bytes(&f);
    let mut omitted = request.clone();
    omitted["decisions"][0]["remaps"]
        .as_array_mut()
        .unwrap()
        .remove(0);
    assert!(stage(&f, &omitted).is_err());
    let mut ambiguous = request.clone();
    for r in ambiguous["decisions"][0]["remaps"].as_array_mut().unwrap() {
        if r["kind"] == "Mention" {
            r["target"]["key"] = json!("right");
        }
    }
    assert!(stage(&f, &ambiguous).is_err());
    assert_eq!(canonical_bytes(&f), before);
    let result = decide(&f, &request);
    let left = &result.allocations[0].entities[&NewEntityKey::new("left").unwrap()];
    let right = &result.allocations[0].entities[&NewEntityKey::new("right").unwrap()];
    assert_ne!(left, right);
    assert_eq!(&bound_entity(&f, "m1"), left);
    let old = note(&f, &source).1.canonical.unwrap();
    assert_eq!(old.string("wiki_status"), Some("superseded"));
    assert!(old.string("wiki_superseded_by_id").is_none());
}
#[test]
fn alias_explicit_no_remap() {
    let f = resolved();
    let source = bound_entity(&f, "m1");
    let artifact = f.artifact();
    let request = alias(&f, &source);
    let mut bad = request.clone();
    bad["decisions"][0]["remaps"] = json!(refs(
        &f,
        &source,
        json!({"kind":"ExistingEntity","entity_id":bound_entity(&f,"m3")})
    ));
    assert!(stage(&f, &bad).is_err());
    decide(&f, &request);
    assert_eq!(f.artifact(), artifact);
    assert!(
        note(&f, &source)
            .1
            .canonical
            .unwrap()
            .field("aliases")
            .unwrap()
            .as_array()
            .unwrap()
            .contains(&json!("Explicit Ada alias"))
    );
    let target = bound_entity(&f, "m3");
    decide(&f, &merge(&f, &source, &target));
    f.catalog.rebuild(&f.writer()).unwrap();
    assert!(load_extraction(&f.view(), &f.extraction).is_ok());
    assert!(
        note(&f, &source)
            .1
            .canonical
            .unwrap()
            .field("aliases")
            .unwrap()
            .as_array()
            .unwrap()
            .contains(&json!("Explicit Ada alias"))
    );
}
#[test]
fn supersession_cycles_and_conflicting_decisions_invalid() {
    let f = resolved();
    let source = bound_entity(&f, "m1");
    let target = bound_entity(&f, "m3");
    let request = merge(&f, &source, &target);
    let mut conflicting = request.clone();
    conflicting["decisions"]
        .as_array_mut()
        .unwrap()
        .push(request["decisions"][0].clone());
    assert!(stage(&f, &conflicting).is_err());
    let result = decide(&f, &request);
    let task = validate_entity_decisions(&f.view(), &serde_json::to_vec(&request).unwrap())
        .unwrap()
        .task_id()
        .clone();
    let main = &result.allocations[0].decision_id;
    let replacement = &result.allocations[0].mention_decisions[0].decision_id;
    let (path, n) = note(&f, main);
    let bytes = edit_note(
        &n,
        &BTreeMap::from([
            ("wiki_supersedes_id".into(), json!(main)),
            ("wiki_supersedes".into(), json!(format!("[[{path}]]"))),
        ]),
        None,
        &n.source_hash,
    )
    .unwrap();
    fs::write(f.root.path().join(path.as_str()), bytes).unwrap();
    f.catalog.rebuild(&f.writer()).unwrap();
    let snapshot = f.catalog.index_snapshot().unwrap();
    assert_eq!(
        snapshot.projection().records[main].eligibility,
        Eligibility::Invalid
    );
    assert!(load_entity_decision_receipt(&f.view(), &task).is_err());
    assert!(snapshot.projection().records.contains_key(replacement));
}
#[test]
fn no_silent_assertion_redirect_retarget() {
    let f = resolved();
    let source = bound_entity(&f, "m1");
    let target = bound_entity(&f, "m3");
    let a = assertion(&f, "a1");
    let (path, n) = note(&f, &a);
    let (target_path, _) = note(&f, &target);
    let proposed = edit_note(
        &n,
        &BTreeMap::from([
            ("wiki_subject_id".into(), json!(target)),
            ("wiki_subject".into(), json!(format!("[[{target_path}]]"))),
        ]),
        None,
        &n.source_hash,
    )
    .unwrap();
    let before = canonical_bytes(&f);
    let p = f
        .engine
        .prepare(
            &f.writer(),
            ChangeDraft {
                title: "Unauthorized endpoint fixture".into(),
                origin: None,
                inverse_of: None,
                allocated_ids: BTreeMap::new(),
                read_preconditions: vec![],
                operations: vec![ExpectedWrite {
                    target: path,
                    expected: ExpectedState::Hash(n.source_hash),
                    proposed: Some(proposed),
                    apply_after: vec![],
                }],
            },
        )
        .unwrap();
    assert!(
        f.engine
            .apply(&f.writer(), &p.prepared, &CatalogGraphValidator, &f.catalog)
            .is_err()
    );
    assert_eq!(canonical_bytes(&f), before);
    f.engine.abort(&f.writer(), &p.prepared).unwrap();
    assert_eq!(bound_entity(&f, "m1"), source);
    decide(&f, &merge(&f, &source, &target));
    assert_eq!(bound_entity(&f, "m1"), target);
}
#[test]
fn entity_decisions_rebuild_exact_ids() {
    let f = resolved();
    let source = bound_entity(&f, "m1");
    let request = split(&f, &source);
    let out = decide(&f, &request);
    let artifact = f.artifact();
    let before = canonical_bytes(&f);
    fs::remove_dir_all(f.root.path().join(".wiki")).unwrap();
    fs::remove_dir_all(f.root.path().join("changes")).unwrap();
    f.catalog.rebuild(&f.writer()).unwrap();
    let restored = stage(&f, &request).unwrap();
    assert!(restored.reused);
    assert!(restored.prepared.is_none());
    assert!(restored.status.is_none());
    assert_eq!(
        restored.disposition,
        EntityDecisionDisposition::CanonicalRestored
    );
    assert_eq!(restored.allocations, out.allocations);
    assert_eq!(f.artifact(), artifact);
    assert_eq!(canonical_bytes(&f), before);
}
#[test]
fn strict_bounds_unknown_null_duplicate_and_dry_plan() {
    let f = resolved();
    let source = bound_entity(&f, "m1");
    let request = alias(&f, &source);
    let before = canonical_bytes(&f);
    let validated =
        validate_entity_decisions(&f.view(), &serde_json::to_vec(&request).unwrap()).unwrap();
    let plan = plan_entity_decisions(&validated).unwrap();
    assert_eq!(plan.summary.add_aliases, 1);
    assert_eq!(canonical_bytes(&f), before);
    for key in ["unknown", "source_ids"] {
        let mut bad = request.clone();
        bad["decisions"][0][key] = json!(null);
        assert!(stage(&f, &bad).is_err());
    }
    let mut null = request.clone();
    null["decisions"][0]["reason"] = Value::Null;
    assert!(stage(&f, &null).is_err());
    let mut huge = request.clone();
    huge["decisions"] = json!(vec![request["decisions"][0].clone(); 17]);
    assert!(stage(&f, &huge).is_err());
    let duplicated = serde_json::to_string(&request).unwrap().replacen(
        "\"schema\":",
        &format!("\"schema\":\"{}\",\"schema\":", ENTITY_DECISIONS_SCHEMA),
        1,
    );
    assert!(validate_entity_decisions(&f.view(), duplicated.as_bytes()).is_err());
    assert_eq!(canonical_bytes(&f), before);
}
#[test]
fn retained_retry_allocates_once_and_stale_inputs_refuse() {
    let f = resolved();
    let source = bound_entity(&f, "m1");
    let request = split(&f, &source);
    let first = stage(&f, &request).unwrap();
    let second = stage(&f, &request).unwrap();
    assert!(second.reused);
    assert_eq!(first.prepared, second.prepared);
    assert_eq!(first.allocations, second.allocations);
    let mut different = request.clone();
    different["decisions"][0]["reason"] = json!("Different authority");
    assert!(stage(&f, &different).is_err());
    let (path, n) = note(&f, &source);
    let changed = edit_note(
        &n,
        &BTreeMap::from([("title".into(), json!("External edit"))]),
        None,
        &n.source_hash,
    )
    .unwrap();
    fs::write(f.root.path().join(path.as_str()), changed).unwrap();
    assert!(
        f.engine
            .apply(
                &f.writer(),
                first.prepared.as_ref().unwrap(),
                &CatalogGraphValidator,
                &f.catalog
            )
            .is_err()
    );
    assert!(stage(&f, &request).is_err());
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
#[test]
fn obsolete_acceptance_entire_scope_resets_or_refuses() {
    for mixed in [false, true] {
        let f = resolved();
        let a = assertion(&f, "a1");
        let b = assertion(&f, "a2");
        for id in if mixed { vec![&a, &b] } else { vec![&a] } {
            f.edit_status(note(&f, id).0, "accepted");
        }
        let decision = accept_decision(
            &f,
            &if mixed {
                vec![a.clone(), b]
            } else {
                vec![a.clone()]
            },
        );
        let source = bound_entity(&f, "m1");
        let target = bound_entity(&f, "m3");
        let before = canonical_bytes(&f);
        let request = merge(&f, &source, &target);
        if mixed {
            assert!(stage(&f, &request).is_err());
            assert_eq!(canonical_bytes(&f), before);
        } else {
            decide(&f, &request);
            assert_eq!(
                note(&f, &decision)
                    .1
                    .canonical
                    .unwrap()
                    .string("wiki_status"),
                Some("superseded")
            );
            assert_eq!(
                note(&f, &a).1.canonical.unwrap().string("wiki_status"),
                Some("proposed")
            );
            assert_ne!(
                f.catalog.index_snapshot().unwrap().projection().records[&a].eligibility,
                Eligibility::Current
            );
        }
    }
    let f = resolved();
    let a = assertion(&f, "a1");
    let b = assertion(&f, "a2");
    for id in [&a, &b] {
        f.edit_status(note(&f, id).0, "accepted");
    }
    let prior = accept_decision(&f, &[a, b]);
    let source = bound_entity(&f, "m1");
    let other = bound_entity(&f, "m3");
    let target = f.existing();
    let mut request = merge(&f, &source, &target);
    request["decisions"][0]["source_ids"] = json!([source, other]);
    let mut remaps = refs(
        &f,
        &source,
        json!({"kind":"ExistingEntity","entity_id":target}),
    );
    remaps.extend(refs(
        &f,
        &other,
        json!({"kind":"ExistingEntity","entity_id":target}),
    ));
    request["decisions"][0]["remaps"] = json!(remaps);
    decide(&f, &request);
    assert_eq!(
        note(&f, &prior).1.canonical.unwrap().string("wiki_status"),
        Some("superseded")
    );
}
#[test]
fn new_referring_note_after_stage_cannot_hide_in_overlay() {
    let f = resolved();
    let source = bound_entity(&f, "m1");
    let target = bound_entity(&f, "m3");
    let staged = stage(&f, &merge(&f, &source, &target)).unwrap();
    let (_, original) = note(&f, &assertion(&f, "a1"));
    let mut fields = original.canonical.unwrap().fields().clone();
    let extra = id("assertion_external_added");
    fields.insert("wiki_id".into(), json!(extra));
    let path = record_path("assertions", &extra);
    fs::write(
        f.root.path().join(path.as_str()),
        authored(
            CanonicalRecord::new(fields).unwrap(),
            "New external reference.\n",
        ),
    )
    .unwrap();
    let before = canonical_bytes(&f);
    assert!(
        f.engine
            .apply(
                &f.writer(),
                staged.prepared.as_ref().unwrap(),
                &CatalogGraphValidator,
                &f.catalog
            )
            .is_err()
    );
    assert_eq!(canonical_bytes(&f), before);
    assert_eq!(bound_entity(&f, "m1"), source);
}
#[test]
fn sequential_merges_same_target_and_original_import_resolution_acknowledge() {
    let f = Fixture::new();
    let original = f.request(creates());
    let initial = f.resolve(&original);
    let target = bound_entity(&f, "m3");
    let first = bound_entity(&f, "m1");
    let second = bound_entity(&f, "m4");
    decide(&f, &alias(&f, &target));
    decide(&f, &merge(&f, &first, &target));
    decide(&f, &merge(&f, &second, &target));
    assert_eq!(bound_entity(&f, "m1"), target);
    assert_eq!(bound_entity(&f, "m4"), target);
    let before = canonical_bytes(&f);
    let retry = f.stage(&original);
    assert_eq!(retry.allocations, initial.allocations);
    assert!(retry.reused);
    let packet = load_packet(&f.view(), &f.artifact().packet_id).unwrap();
    let validated = validate_response(&packet, &f.view(), &f.raw).unwrap();
    let imported = stage_import(
        &f.engine,
        &f.writer(),
        &validated,
        OriginPolicy::ReuseOrConflict,
    )
    .unwrap();
    assert!(imported.reused);
    assert_eq!(
        imported.extraction.record.as_ref().unwrap().record_id,
        f.extraction
    );
    assert_eq!(canonical_bytes(&f), before);
    f.catalog.rebuild(&f.writer()).unwrap();
    assert!(load_extraction(&f.view(), &f.extraction).is_ok());
}
#[test]
fn published_wire_and_receipt_schemas_match_real_strict_data() {
    let f = resolved();
    let source = bound_entity(&f, "m1");
    let request = split(&f, &source);
    let wire: Value =
        serde_json::from_str(include_str!("../schemas/entity-decisions-v1.json")).unwrap();
    let validator = jsonschema::validator_for(&wire).unwrap();
    validator.validate(&request).unwrap();
    let out = decide(&f, &request);
    let main = &out.allocations[0].decision_id;
    let (path, _) = note(&f, main);
    let current = SourceView::from_fs_bounded(f.engine.fs(), 64 * 1024 * 1024, 4096).unwrap();
    let v = validate_entity_decisions(&current, &serde_json::to_vec(&request).unwrap()).unwrap();
    let receipt = load_entity_decision_receipt(&current, v.task_id())
        .unwrap()
        .unwrap();
    let encoded = serde_json::to_value(receipt.receipt()).unwrap();
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/entity-decision-receipt-v1.json")).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&encoded)
        .unwrap();
    let mut invalid = request.clone();
    invalid["decisions"][0]["unknown"] = json!(true);
    assert!(!validator.is_valid(&invalid));
    assert!(stage(&f, &invalid).is_err());
    let mut malformed = encoded;
    malformed["allocations"][0]["unknown"] = json!(null);
    assert!(
        !jsonschema::validator_for(&schema)
            .unwrap()
            .is_valid(&malformed)
    );
    assert!(path.as_str().contains("decisions"));
}
#[test]
fn partial_recovery_uses_real_retained_original_witness() {
    use std::{
        fs::File,
        io,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };
    struct StopAfterAssertion(AtomicBool);
    impl DurableIo for StopAfterAssertion {
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
            if t.parent()
                .is_some_and(|p| p.ends_with("knowledge/assertions"))
                && !self.0.swap(true, Ordering::SeqCst)
            {
                return Err(io::Error::other(
                    "real post assertion replacement interruption",
                ));
            }
            Ok(())
        }
        fn remove(&self, p: &Path) -> io::Result<()> {
            NativeIo.remove(p)
        }
        fn create_directory(&self, p: &Path) -> io::Result<()> {
            NativeIo.create_directory(p)
        }
        fn sync_directory(&self, p: &Path) -> io::Result<DirectorySync> {
            NativeIo.sync_directory(p)
        }
    }
    for added_reference in [false, true] {
        let f = resolved();
        let source = bound_entity(&f, "m1");
        let target = bound_entity(&f, "m3");
        let staged = stage(&f, &merge(&f, &source, &target)).unwrap();
        let assertion_id = assertion(&f, "a1");
        let original_assertion = note(&f, &assertion_id).1.canonical.unwrap();
        let io = Arc::new(StopAfterAssertion(AtomicBool::new(false)));
        let interrupted = ChangeEngine::new(VaultFs::with_io(f.root.clone(), io.clone())).unwrap();
        assert!(
            interrupted
                .apply(
                    &f.writer(),
                    staged.prepared.as_ref().unwrap(),
                    &CatalogGraphValidator,
                    &f.catalog
                )
                .is_err()
        );
        assert!(io.0.load(Ordering::SeqCst));
        let a = note(&f, &assertion_id).1.canonical.unwrap();
        assert_eq!(a.string("wiki_subject_id"), Some(target.as_str()));
        assert_eq!(
            note(&f, &source).1.canonical.unwrap().string("wiki_status"),
            Some("active")
        );
        if added_reference {
            let extra = id("assertion_added_during_partial_recovery");
            let mut fields = original_assertion.fields().clone();
            fields.insert("wiki_id".into(), json!(extra));
            fs::write(
                f.root
                    .path()
                    .join(record_path("assertions", &extra).as_str()),
                authored(
                    CanonicalRecord::new(fields).unwrap(),
                    "New author reference after interrupted replace.\n",
                ),
            )
            .unwrap();
            let before = canonical_bytes(&f);
            assert!(
                f.engine
                    .recover(&f.writer(), &CatalogGraphValidator, &f.catalog)
                    .is_err()
            );
            assert_eq!(canonical_bytes(&f), before);
            continue;
        }
        let report = f
            .engine
            .recover(&f.writer(), &CatalogGraphValidator, &f.catalog)
            .unwrap();
        assert!(
            report
                .changes
                .iter()
                .any(|c| c.status == ChangeStatus::Committed)
        );
        assert_eq!(bound_entity(&f, "m1"), target);
    }
}

fn replace_fence(note: &lwiki::records::ParsedNote, fence: &str, value: &Value) -> Vec<u8> {
    let text = std::str::from_utf8(note.body()).unwrap();
    let marker = format!("```{fence}\n");
    let start = text.find(&marker).unwrap() + marker.len();
    let end = start + text[start..].find("\n```").unwrap();
    let mut body = text.as_bytes()[..start].to_vec();
    body.extend(canonical_json(value).unwrap());
    body.extend_from_slice(&text.as_bytes()[end..]);
    edit_note(note, &BTreeMap::new(), Some(&body), &note.source_hash).unwrap()
}
#[test]
fn sealed_overlay_rejects_hidden_object_even_with_forged_matching_hash_claims() {
    let f = resolved();
    let source = bound_entity(&f, "m1");
    let target = bound_entity(&f, "m3");
    let request = merge(&f, &source, &target);
    let out = stage(&f, &request).unwrap();
    let change = f.engine.inspect(&out.prepared.unwrap().change_id).unwrap();
    let assertion_id = assertion(&f, "a1");
    let hidden = bound_entity(&f, "m5");
    let (hidden_path, _) = note(&f, &hidden);
    let mut payloads = vec![];
    let mut receipt = None;
    let app = lwiki::app::OfflineApp::new(
        f.engine.fs().clone(),
        lwiki::app::OperationOptions {
            offline: true,
            ..lwiki::app::OperationOptions::default()
        },
    )
    .unwrap();
    for (i, op) in change.manifest.operations.iter().enumerate() {
        let bytes = app
            .changes_payload(change.prepared.change_id.clone(), i)
            .unwrap()
            .proposed
            .unwrap();
        let n = parse_note(&bytes);
        if n.canonical
            .as_ref()
            .is_some_and(|r| r.id() == &out.allocations[0].decision_id)
        {
            let text = std::str::from_utf8(n.body()).unwrap();
            let start = text.find("```lwiki-entity-decisions-v1\n").unwrap()
                + "```lwiki-entity-decisions-v1\n".len();
            let end = start + text[start..].find("\n```").unwrap();
            receipt = Some(serde_json::from_str::<Value>(&text[start..end]).unwrap());
        }
        payloads.push((op.clone(), bytes));
    }
    let mut receipt = receipt.unwrap();
    for (op, bytes) in &mut payloads {
        let n = parse_note(bytes);
        if n.canonical
            .as_ref()
            .is_some_and(|r| r.id() == &assertion_id)
        {
            *bytes = edit_note(
                &n,
                &BTreeMap::from([
                    ("wiki_object_id".into(), json!(hidden)),
                    ("wiki_object".into(), json!(format!("[[{hidden_path}]]"))),
                ]),
                None,
                &n.source_hash,
            )
            .unwrap();
            for proof in receipt["operations"].as_array_mut().unwrap() {
                if proof["target"] == json!(op.target) {
                    proof["after"] = json!(ExpectedState::Hash(Blake3Hash::digest(&*bytes)));
                }
            }
        }
    }
    let operations = payloads
        .into_iter()
        .map(|(op, bytes)| {
            let n = parse_note(&bytes);
            let bytes = if n.canonical.as_ref().is_some_and(|r| {
                out.allocations.iter().any(|a| {
                    &a.decision_id == r.id()
                        || a.mention_decisions.iter().any(|m| &m.decision_id == r.id())
                })
            }) {
                replace_fence(&n, ENTITY_DECISION_FENCE, &receipt)
            } else {
                bytes
            };
            ExpectedWrite {
                target: op.target,
                expected: op.before,
                proposed: Some(bytes),
                apply_after: vec![],
            }
        })
        .collect();
    let forged = f
        .engine
        .prepare(
            &f.writer(),
            ChangeDraft {
                title: "Forged hidden endpoint fixture".into(),
                origin: None,
                inverse_of: None,
                allocated_ids: change.manifest.allocated_ids,
                read_preconditions: change.manifest.read_preconditions,
                operations,
            },
        )
        .unwrap();
    let before = canonical_bytes(&f);
    assert!(
        f.engine
            .apply(
                &f.writer(),
                &forged.prepared,
                &CatalogGraphValidator,
                &f.catalog
            )
            .is_err()
    );
    assert_eq!(canonical_bytes(&f), before);
    f.engine.abort(&f.writer(), &forged.prepared).unwrap();
}
#[test]
fn shared_predecessor_partition_is_explicit_and_complete() {
    let f = Fixture::new();
    let mut mappings = creates();
    mappings.retain(|v| v["mention_id"] != "m3");
    f.resolve(&f.request(mappings));
    let source = bound_entity(&f, "m1");
    f.resolve(&f.request(vec![f.bind("m3", &source)]));
    let original = f.artifact();
    let MentionBinding::Resolved {
        decision_id: first, ..
    } = &original.bindings[&local("m1")]
    else {
        panic!()
    };
    let MentionBinding::Resolved {
        decision_id: second,
        ..
    } = &original.bindings[&local("m3")]
    else {
        panic!()
    };
    let (first_path, first_note) = note(&f, first);
    let (second_path, second_note) = note(&f, second);
    let (extraction_path, extraction_note) = note(&f, &f.extraction);
    let mut shared = original.clone();
    shared.bindings.insert(
        local("m3"),
        MentionBinding::Resolved {
            entity_id: source.clone(),
            decision_id: first.clone(),
        },
    );
    let writes = vec![
        (
            first_path,
            first_note.clone(),
            edit_note(
                &first_note,
                &BTreeMap::from([
                    ("wiki_mention_ids".into(), json!(["m1", "m3"])),
                    ("wiki_supersedes_id".into(), json!(second)),
                    (
                        "wiki_supersedes".into(),
                        json!(format!("[[{second_path}]]")),
                    ),
                ]),
                None,
                &first_note.source_hash,
            )
            .unwrap(),
        ),
        (
            second_path,
            second_note.clone(),
            edit_note(
                &second_note,
                &BTreeMap::from([("wiki_status".into(), json!("superseded"))]),
                None,
                &second_note.source_hash,
            )
            .unwrap(),
        ),
        (
            extraction_path,
            extraction_note.clone(),
            replace_fence(
                &extraction_note,
                ARTIFACT_FENCE,
                &serde_json::to_value(&shared).unwrap(),
            ),
        ),
    ];
    f.apply(ChangeDraft {
        title: "Explicit legacy shared mention authority fixture".into(),
        origin: None,
        inverse_of: None,
        allocated_ids: BTreeMap::new(),
        read_preconditions: vec![],
        operations: writes
            .into_iter()
            .map(|(path, n, b)| ExpectedWrite {
                target: path,
                expected: ExpectedState::Hash(n.source_hash),
                proposed: Some(b),
                apply_after: vec![],
            })
            .collect(),
    });
    let request = split(&f, &source);
    let mut incomplete = request.clone();
    incomplete["decisions"][0]["remaps"]
        .as_array_mut()
        .unwrap()
        .retain(|v| v["mention_id"] != "m3");
    let before = canonical_bytes(&f);
    assert!(stage(&f, &incomplete).is_err());
    assert_eq!(canonical_bytes(&f), before);
    let result = decide(&f, &request);
    assert_eq!(result.allocations[0].mention_decisions.len(), 2);
    assert!(
        result.allocations[0]
            .mention_decisions
            .iter()
            .all(|m| &m.predecessor_id == first)
    );
    assert_eq!(f.artifact().raw_response, original.raw_response);
}
#[test]
fn canonical_only_restore_preserves_renames_prose_and_later_review() {
    use lwiki::app::{OfflineApp, OperationOptions};
    let f = resolved();
    let source = bound_entity(&f, "m1");
    let request = split(&f, &source);
    let out = decide(&f, &request);
    let entity = &out.allocations[0].entities[&NewEntityKey::new("left").unwrap()];
    let app = OfflineApp::new(
        f.engine.fs().clone(),
        OperationOptions {
            offline: true,
            ..OperationOptions::default()
        },
    )
    .unwrap();
    for (id, to) in [
        (entity.clone(), "knowledge/entities/author-left.md"),
        (
            f.extraction.clone(),
            "knowledge/extractions/author-extraction.md",
        ),
    ] {
        let (_, n) = note(&f, &id);
        app.page_rename(id, VaultRelativePath::new(to).unwrap(), n.source_hash)
            .unwrap();
    }
    let (path, n) = note(&f, entity);
    let mut body = n.body().to_vec();
    body.extend_from_slice(b"\nAuthor prose preserved through receipt acknowledgement.\n");
    let bytes = edit_note(
        &n,
        &BTreeMap::from([("title".into(), json!("Author entity title"))]),
        Some(&body),
        &n.source_hash,
    )
    .unwrap();
    f.apply(ChangeDraft {
        title: "Author identity preserving edit".into(),
        origin: None,
        inverse_of: None,
        allocated_ids: BTreeMap::new(),
        read_preconditions: vec![],
        operations: vec![ExpectedWrite {
            target: path,
            expected: ExpectedState::Hash(n.source_hash),
            proposed: Some(bytes),
            apply_after: vec![],
        }],
    });
    f.edit_status(note(&f, &assertion(&f, "a1")).0, "accepted");
    let retry = stage(&f, &request).unwrap();
    assert!(retry.reused);
    assert_eq!(retry.allocations, out.allocations);
    fs::remove_dir_all(f.root.path().join(".wiki")).unwrap();
    fs::remove_dir_all(f.root.path().join("changes")).unwrap();
    let before = canonical_bytes(&f);
    let restored = stage(&f, &request).unwrap();
    assert!(restored.prepared.is_none());
    assert_eq!(restored.allocations, out.allocations);
    assert_eq!(canonical_bytes(&f), before);
    assert_eq!(
        note(&f, &assertion(&f, "a1"))
            .1
            .canonical
            .unwrap()
            .string("wiki_status"),
        Some("accepted")
    );
}

fn retained_draft(f: &Fixture, out: &EntityDecisionOutcome) -> ChangeDraft {
    let prepared = out.prepared.as_ref().unwrap();
    let change = f.engine.inspect(&prepared.change_id).unwrap();
    let app = lwiki::app::OfflineApp::new(
        f.engine.fs().clone(),
        lwiki::app::OperationOptions {
            offline: true,
            ..Default::default()
        },
    )
    .unwrap();
    let operations = change
        .manifest
        .operations
        .iter()
        .enumerate()
        .map(|(i, op)| ExpectedWrite {
            target: op.target.clone(),
            expected: op.before.clone(),
            proposed: app
                .changes_payload(prepared.change_id.clone(), i)
                .unwrap()
                .proposed,
            apply_after: op
                .apply_after
                .iter()
                .map(|i| change.manifest.operations[*i].target.clone())
                .collect(),
        })
        .collect();
    ChangeDraft {
        title: "Direct retained authority forgery fixture".into(),
        origin: change.manifest.origin,
        inverse_of: None,
        allocated_ids: change.manifest.allocated_ids,
        read_preconditions: change.manifest.read_preconditions,
        operations,
    }
}

#[test]
fn retained_apply_rejects_unlisted_decision_write() {
    let f = resolved();
    let out = stage(
        &f,
        &merge(&f, &bound_entity(&f, "m1"), &bound_entity(&f, "m3")),
    )
    .unwrap();
    let mut draft = retained_draft(&f, &out);
    // prepare with an existing identical origin deliberately reuses that
    // manifest. Remove only this aborted disposable proposal so the forged
    // draft below is genuinely retained and reaches apply's witness hook.
    f.engine
        .abort(&f.writer(), out.prepared.as_ref().unwrap())
        .unwrap();
    fs::remove_dir_all(
        f.root
            .path()
            .join("changes")
            .join(out.prepared.as_ref().unwrap().change_id.as_str()),
    )
    .unwrap();
    let extra = RecordId::generate(RecordKind::Decision).unwrap();
    let entity = bound_entity(&f, "m5");
    let record = CanonicalRecord::new(BTreeMap::from([
        ("wiki_schema".into(), json!("1")),
        ("wiki_id".into(), json!(extra)),
        ("wiki_kind".into(), json!("decision")),
        ("title".into(), json!("Unrelated valid alias decision")),
        ("wiki_status".into(), json!("active")),
        ("wiki_action".into(), json!("add_alias")),
        ("wiki_created_at".into(), json!("2026-09-28T00:00:00Z")),
        ("wiki_input_ids".into(), json!([entity])),
        ("wiki_output_ids".into(), json!([entity])),
    ]))
    .unwrap();
    draft.operations.push(ExpectedWrite {
        target: record_path("decisions", &extra),
        expected: ExpectedState::Absent,
        proposed: Some(authored(record, "Unrelated explicit alias rationale.\n")),
        apply_after: vec![],
    });
    let forged = f.engine.prepare(&f.writer(), draft).unwrap();
    assert!(
        forged
            .manifest
            .operations
            .iter()
            .any(|op| op.target == record_path("decisions", &extra))
    );
    let before = canonical_bytes(&f);
    let error = f
        .engine
        .apply(
            &f.writer(),
            &forged.prepared,
            &CatalogGraphValidator,
            &f.catalog,
        )
        .unwrap_err();
    assert!(
        error.message.contains("exact write membership"),
        "{error:?}"
    );
    assert_eq!(canonical_bytes(&f), before);
    f.engine.abort(&f.writer(), &forged.prepared).unwrap();
}

#[test]
fn retained_alias_apply_requires_exact_origin_receipt_match() {
    for missing_receipt in [false, true] {
        let f = resolved();
        let out = stage(&f, &alias(&f, &bound_entity(&f, "m1"))).unwrap();
        let mut draft = retained_draft(&f, &out);
        draft.origin.as_mut().unwrap().packet_id = id("entity_decisions_unmatched_origin");
        if missing_receipt {
            let creation = draft
                .operations
                .iter_mut()
                .find(|op| {
                    op.proposed.as_ref().is_some_and(|bytes| {
                        parse_note(bytes)
                            .canonical
                            .as_ref()
                            .is_some_and(|r| r.id() == &out.allocations[0].decision_id)
                    })
                })
                .unwrap();
            let note = parse_note(creation.proposed.as_ref().unwrap());
            creation.proposed = Some(
                edit_note(
                    &note,
                    &BTreeMap::new(),
                    Some(b"Explicit alias rationale with no durable receipt.\n"),
                    &note.source_hash,
                )
                .unwrap(),
            );
        }
        let forged = f.engine.prepare(&f.writer(), draft).unwrap();
        let before = canonical_bytes(&f);
        let error = f
            .engine
            .apply(
                &f.writer(),
                &forged.prepared,
                &CatalogGraphValidator,
                &f.catalog,
            )
            .unwrap_err();
        assert!(
            error.message.contains("match exactly one receipt"),
            "{error:?}"
        );
        assert_eq!(canonical_bytes(&f), before);
        f.engine.abort(&f.writer(), &forged.prepared).unwrap();
        f.engine
            .abort(&f.writer(), out.prepared.as_ref().unwrap())
            .unwrap();
    }
}

#[test]
fn reversed_sort_receipt_chain_enforces_64_hop_ceiling() {
    // Real canonical receipts/typed outcomes exercise the production policy;
    // no filesystem changes or imitation of its graph traversal are needed.
    fn chain(count: usize) -> BTreeMap<VaultRelativePath, lwiki::records::ParsedNote> {
        let mut notes = BTreeMap::new();
        let entity = |i| id(&format!("entity_chain_{i:03}"));
        let decision = |i| id(&format!("decision_chain_{i:03}"));
        for i in 0..=count {
            let mut fields = BTreeMap::from([
                ("wiki_schema".into(), json!("1")),
                ("wiki_id".into(), json!(entity(i))),
                ("wiki_kind".into(), json!("entity")),
                ("title".into(), json!(format!("Identity {i}"))),
                (
                    "wiki_status".into(),
                    json!(if i == 0 { "active" } else { "superseded" }),
                ),
                ("wiki_entity_type".into(), json!("concept")),
            ]);
            if i != 0 {
                fields.insert("wiki_superseded_by_id".into(), json!(entity(i - 1)));
            }
            let bytes = authored(
                CanonicalRecord::new(fields).unwrap(),
                "Explicit identity.\n",
            );
            notes.insert(record_path("entities", &entity(i)), parse_note(&bytes));
        }
        for i in 0..count {
            let mut expected = vec![
                ExpectedRecord {
                    record_id: entity(i),
                    hash: Blake3Hash::digest(b"original target"),
                },
                ExpectedRecord {
                    record_id: entity(i + 1),
                    hash: Blake3Hash::digest(b"original source"),
                },
            ];
            if i + 1 < count {
                expected.push(ExpectedRecord {
                    record_id: decision(i + 1),
                    hash: Blake3Hash::digest(b"original predecessor"),
                });
            }
            expected.sort_by(|a, b| a.record_id.cmp(&b.record_id));
            let request = EntityDecisionRequest {
                schema: ENTITY_DECISIONS_SCHEMA.into(),
                decisions: vec![EntityDecision::MergeEntities {
                    reason: "Explicit entity-only chain".into(),
                    expected_records: expected.clone(),
                    remaps: vec![],
                    source_ids: vec![entity(i + 1)],
                    target_id: entity(i),
                }],
            };
            let request_hash = Blake3Hash::digest(canonical_json(&request).unwrap());
            let scope = json!([{"action":"merge", "sources":[entity(i + 1)], "expected_records": expected}]);
            let scope_hash = Blake3Hash::digest(canonical_json(&scope).unwrap());
            let task_id = id(&format!(
                "entity_decisions_{}",
                scope_hash.as_str().strip_prefix("blake3:").unwrap()
            ));
            let mut paths = expected
                .iter()
                .map(|e| {
                    (
                        e.record_id.clone(),
                        if e.record_id == decision(i + 1) {
                            record_path("decisions", &e.record_id)
                        } else {
                            record_path("entities", &e.record_id)
                        },
                    )
                })
                .collect::<BTreeMap<_, _>>();
            paths.insert(decision(i), record_path("decisions", &decision(i)));
            let source_path = record_path("entities", &entity(i + 1));
            let receipt = EntityDecisionReceiptV1 {
                schema: ENTITY_DECISION_RECEIPT_SCHEMA.into(),
                task_id,
                request,
                request_hash,
                allocations: vec![EntityDecisionAllocation {
                    decision_id: decision(i),
                    entities: BTreeMap::new(),
                    mention_decisions: vec![],
                }],
                record_paths: paths,
                extraction_proofs: vec![],
                assertion_proofs: vec![],
                main_supersessions: if i + 1 < count {
                    vec![MainDecisionSupersession {
                        predecessor_id: decision(i + 1),
                        successor_id: decision(i),
                    }]
                } else {
                    vec![]
                },
                operations: vec![EntityDecisionWriteProof {
                    target: source_path.clone(),
                    before: ExpectedState::Hash(Blake3Hash::digest(b"original source")),
                    after: ExpectedState::Hash(notes[&source_path].source_hash.clone()),
                }],
            };
            let mut fields = BTreeMap::from([
                ("wiki_schema".into(), json!("1")),
                ("wiki_id".into(), json!(decision(i))),
                ("wiki_kind".into(), json!("decision")),
                ("title".into(), json!("Explicit entity merge")),
                (
                    "wiki_status".into(),
                    json!(if i == 0 { "active" } else { "superseded" }),
                ),
                ("wiki_action".into(), json!("merge")),
                ("wiki_created_at".into(), json!("2026-09-28T00:00:00Z")),
                ("wiki_input_ids".into(), json!([entity(i + 1)])),
                ("wiki_output_ids".into(), json!([entity(i)])),
            ]);
            if i + 1 < count {
                fields.insert("wiki_supersedes_id".into(), json!(decision(i + 1)));
            }
            let body = format!(
                "Explicit merge.\n\n```{ENTITY_DECISION_FENCE}\n{}\n```\n",
                String::from_utf8(canonical_json(&receipt).unwrap()).unwrap()
            );
            let bytes = authored(CanonicalRecord::new(fields).unwrap(), &body);
            notes.insert(record_path("decisions", &decision(i)), parse_note(&bytes));
        }
        notes
    }
    assert!(verify_decision_policy(&chain(65)).unwrap().is_some());
    let error = verify_decision_policy(&chain(67)).unwrap_err();
    assert!(error.message.contains("chain ceiling"), "{error:?}");
}

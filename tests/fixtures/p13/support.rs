#![allow(dead_code)]
pub(crate) use lwiki::{
    catalog::*,
    changes::*,
    domain::*,
    graph::{
        decision_types::*, decisions::*, extraction_types::*, import::*, packet::*, resolution::*,
        resolution_types::*, review::*, review_types::*, wire::*,
    },
    records::{edit_note, parse_note},
    sources::*,
    vault::*,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path, time::Duration};
const VAULT: &str = "vault_00000000-0000-7000-8000-00000000001b";
pub(crate) fn id(s: &str) -> RecordId {
    RecordId::new(s).unwrap()
}
pub(crate) fn local(s: &str) -> PacketLocalId {
    PacketLocalId::new(s).unwrap()
}
pub(crate) fn copy(from: &Path, to: &Path) {
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
pub(crate) struct Fixture {
    _temp: tempfile::TempDir,
    pub(crate) root: VaultRoot,
    pub(crate) engine: ChangeEngine,
    pub(crate) catalog: Catalog,
    pub(crate) source: RecordId,
    pub(crate) extraction: RecordId,
    pub(crate) raw: Vec<u8>,
}
impl Fixture {
    pub(crate) fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        copy(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bootstrap/vault"),
            temp.path(),
        );
        let root = VaultRoot::explicit(temp.path()).unwrap();
        let vault = VaultFs::new(root.clone());
        let engine = ChangeEngine::new(vault.clone()).unwrap();
        let catalog = Catalog::new(vault.clone(), id(VAULT));
        let text = include_str!("source.md").replace('\n', "\r\n");
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
    pub(crate) fn writer(&self) -> WriterPermit {
        WriterPermit::acquire(&self.root, Duration::from_secs(2)).unwrap()
    }
    pub(crate) fn view(&self) -> SourceView<'_> {
        SourceView::from_fs_bounded(self.engine.fs(), 64 * 1024 * 1024, 4096).unwrap()
    }
    pub(crate) fn apply(&self, draft: ChangeDraft) {
        let prepared = self.engine.prepare(&self.writer(), draft).unwrap().prepared;
        self.apply_prepared(&prepared);
    }
    pub(crate) fn apply_prepared(&self, prepared: &PreparedChange) {
        self.engine
            .apply(
                &self.writer(),
                prepared,
                &CatalogGraphValidator,
                &self.catalog,
            )
            .unwrap();
    }
    pub(crate) fn artifact(&self) -> ExtractionArtifactV1 {
        load_extraction(&self.view(), &self.extraction)
            .unwrap()
            .artifact()
            .clone()
    }
    pub(crate) fn request(&self, mappings: Vec<Value>) -> Value {
        let loaded = load_extraction(&self.view(), &self.extraction).unwrap();
        json!({"schema":RESOLUTION_SCHEMA,"extraction_id":self.extraction,"expected_hash":loaded.locator().observed_hash,"mappings":mappings})
    }
    pub(crate) fn validate(&self, request: &Value) -> ValidatedResolution {
        validate_resolution(&self.view(), &serde_json::to_vec(request).unwrap()).unwrap()
    }
    pub(crate) fn stage(&self, request: &Value) -> ResolutionOutcome {
        stage_resolution(&self.engine, &self.writer(), &self.validate(request)).unwrap()
    }
    pub(crate) fn resolve(&self, request: &Value) -> ResolutionOutcome {
        let outcome = self.stage(request);
        self.apply_prepared(outcome.prepared.as_ref().unwrap());
        outcome
    }
    pub(crate) fn bind(&self, local: &str, entity: &RecordId) -> Value {
        let (_, parsed) = self.entity_note(entity);
        json!({"operation":"BindMention","mention_id":local,"reason":"Explicit fixture identity","entity_id":entity,"expected_entity_hash":parsed.source_hash})
    }
    pub(crate) fn entity_note(
        &self,
        entity: &RecordId,
    ) -> (VaultRelativePath, lwiki::records::ParsedNote) {
        for path in self.root.scan_markdown().unwrap() {
            let note = parse_note(&fs::read(self.root.path().join(path.as_str())).unwrap());
            if note.canonical.as_ref().is_some_and(|r| r.id() == entity) {
                return (path, note);
            }
        }
        panic!("missing fixture entity {entity}");
    }
    pub(crate) fn existing(&self) -> RecordId {
        id("entity_00000000-0000-7000-8000-000000000001")
    }
    pub(crate) fn edit_status(&self, path: VaultRelativePath, status: &str) {
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
pub(crate) fn create(mention: &str, title: &str, kind: &str) -> Value {
    json!({"operation":"CreateEntity","mention_id":mention,"reason":"Explicit distinct identity","title":title,"entity_type":kind})
}
pub(crate) fn creates() -> Vec<Value> {
    vec![
        create("m1", "Ada", "person"),
        create("m2", "Acme", "organization"),
        create("m3", "Ada", "person"),
        create("m4", "She", "person"),
        create("m5", "Tool", "component"),
    ]
}
pub(crate) fn record_path(kind: &str, id: &RecordId) -> VaultRelativePath {
    VaultRelativePath::new(format!("knowledge/{kind}/{id}.md")).unwrap()
}

pub(crate) fn resolved() -> Fixture {
    let f = Fixture::new();
    f.resolve(&f.request(creates()));
    f
}
pub(crate) fn bound_entity(f: &Fixture, mention: &str) -> RecordId {
    match &f.artifact().bindings[&local(mention)] {
        MentionBinding::Resolved { entity_id, .. } => entity_id.clone(),
        _ => panic!("resolved fixture endpoint"),
    }
}
pub(crate) fn guards(f: &Fixture) -> Value {
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
pub(crate) fn refs(f: &Fixture, entity: &RecordId, target: Value) -> Vec<Value> {
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
pub(crate) fn merge(f: &Fixture, source: &RecordId, target: &RecordId) -> Value {
    json!({"schema":ENTITY_DECISIONS_SCHEMA,"decisions":[{"operation":"MergeEntities","source_ids":[source],"target_id":target,"reason":"Explicit fixture identity merge","expected_records":guards(f),"remaps":refs(f,source,json!({"kind":"ExistingEntity","entity_id":target}))}]})
}
pub(crate) fn split(f: &Fixture, source: &RecordId) -> Value {
    json!({"schema":ENTITY_DECISIONS_SCHEMA,"decisions":[{"operation":"SplitEntity","source_id":source,"reason":"Explicit exhaustive identity partition","expected_records":guards(f),"new_entities":[{"key":"left","title":"Ada left","entity_type":"person"},{"key":"right","title":"Ada right","entity_type":"person"}],"remaps":refs(f,source,json!({"kind":"NewEntity","key":"left"}))}]})
}
pub(crate) fn alias(f: &Fixture, entity: &RecordId) -> Value {
    json!({"schema":ENTITY_DECISIONS_SCHEMA,"decisions":[{"operation":"AddAlias","entity_id":entity,"alias":"Explicit Ada alias","reason":"Explicit label only","expected_records":guards(f),"remaps":[]}]})
}
pub(crate) fn stage(f: &Fixture, request: &Value) -> Result<EntityDecisionOutcome> {
    let v = validate_entity_decisions(&f.view(), &serde_json::to_vec(request).unwrap())?;
    stage_entity_decisions(&f.engine, &f.writer(), &v)
}
pub(crate) fn decide(f: &Fixture, request: &Value) -> EntityDecisionOutcome {
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
pub(crate) fn canonical_bytes(f: &Fixture) -> BTreeMap<String, Vec<u8>> {
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
pub(crate) fn assertion(f: &Fixture, local_id: &str) -> RecordId {
    f.artifact().allocations.assertions[&local(local_id)].clone()
}
pub(crate) fn note(f: &Fixture, id: &RecordId) -> (VaultRelativePath, lwiki::records::ParsedNote) {
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

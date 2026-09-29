#[path = "../test_support/paths.rs"]
mod test_paths;
use lwiki::{
    catalog::*,
    changes::*,
    domain::*,
    graph::{
        extraction_types::*, import::*, mention_state::*, packet::*, resolution::*,
        resolution_types::*, wire::*,
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
            &test_paths::fixture(env!("CARGO_MANIFEST_DIR"), "tests/fixtures/bootstrap/vault"),
            temp.path(),
        );
        let root = VaultRoot::explicit(temp.path()).unwrap();
        let vault = VaultFs::new(root.clone());
        let engine = ChangeEngine::new(vault.clone()).unwrap();
        let catalog = Catalog::new(vault.clone(), id(VAULT));
        let text = include_str!("fixtures/p11/source.md").replace('\n', "\r\n");
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
fn reject(mention: &str) -> Value {
    json!({"operation":"RejectMention","mention_id":mention,"reason":"No justified identity"})
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

#[test]
fn homonym_and_pronoun_require_explicit_binding() {
    let f = Fixture::new();
    let before = f.artifact();
    assert!(
        before
            .bindings
            .values()
            .all(|b| *b == MentionBinding::Pending)
    );
    let request = f.request(vec![
        create("m1", "Ada", "person"),
        create("m2", "Acme", "organization"),
        create("m3", "Ada", "person"),
        create("m5", "Tool", "component"),
    ]);
    let outcome = f.resolve(&request);
    assert_ne!(
        outcome.allocations.entities[&local("m1")],
        outcome.allocations.entities[&local("m3")]
    );
    let after = f.artifact();
    assert_eq!(after.bindings[&local("m4")], MentionBinding::Pending);
    assert_eq!(
        after.materialized_assertions,
        [local("a1"), local("a2"), local("a4")]
    );
    let entity = &outcome.allocations.entities[&local("m1")];
    let request = f.request(vec![f.bind("m4", entity)]);
    f.resolve(&request);
    assert_eq!(f.artifact().materialized_assertions.len(), 4);
}
#[test]
fn resolution_expected_hash_and_complete_mapping() {
    let f = Fixture::new();
    let original = f.artifact();
    let request = f.request(vec![f.bind("m1", &f.existing())]);
    let mut stale = request.clone();
    stale["expected_hash"] = json!(Blake3Hash::digest(b"wrong"));
    assert!(validate_resolution(&f.view(), &serde_json::to_vec(&stale).unwrap()).is_err());
    let mut stale = request.clone();
    stale["mappings"][0]["expected_entity_hash"] = json!(Blake3Hash::digest(b"wrong"));
    assert!(validate_resolution(&f.view(), &serde_json::to_vec(&stale).unwrap()).is_err());
    let outcome = f.resolve(&request);
    assert_eq!(outcome.summary.pending_mentions, 4);
    assert!(outcome.summary.materialize_assertions.is_empty());
    let after = f.artifact();
    assert_eq!(after.bindings.len(), 5);
    for key in ["m2", "m3", "m4", "m5"] {
        assert_eq!(after.bindings[&local(key)], original.bindings[&local(key)]);
    }
    assert_eq!(after.allocations, original.allocations);
    assert_eq!(after.raw_response, original.raw_response);
}
#[test]
fn rejected_mentions_never_activate_endpoints() {
    let f = Fixture::new();
    let request = f.request(vec![reject("m1"), create("m2", "Acme", "organization")]);
    let outcome = f.resolve(&request);
    let after = f.artifact();
    assert!(matches!(
        after.bindings[&local("m1")],
        MentionBinding::Rejected { .. }
    ));
    assert!(after.materialized_assertions.is_empty());
    for id in after
        .allocations
        .assertions
        .values()
        .chain(after.allocations.evidence.values().flatten())
    {
        assert!(
            !f.root
                .path()
                .join(
                    record_path(
                        if id.as_str().starts_with("assertion_") {
                            "assertions"
                        } else {
                            "evidence"
                        },
                        id
                    )
                    .as_str()
                )
                .exists()
        );
    }
    assert!(
        validate_resolution(
            &f.view(),
            &serde_json::to_vec(&f.request(vec![create("m1", "Ada", "person")])).unwrap()
        )
        .is_err()
    );
    let reused = f.stage(&request);
    assert!(reused.reused);
    assert_eq!(reused.allocations, outcome.allocations);
}
#[test]
fn resolve_apply_preserves_raw_source_local_output() {
    let f = Fixture::new();
    let loaded = load_extraction(&f.view(), &f.extraction).unwrap();
    let path = f.root.path().join(loaded.locator().path.as_str());
    let raw = fs::read(&path).unwrap();
    let mut text = String::from_utf8(raw).unwrap();
    text = text.replacen(
        "title:",
        "custom_author: \"retain me\"\n# author comment\ntitle:",
        1,
    );
    text.push_str("\nUser explanation stays byte-exact.\n");
    fs::write(&path, &text).unwrap();
    let before = f.artifact();
    let request = f.request(creates());
    let outcome = f.stage(&request);
    assert_eq!(fs::read(&path).unwrap(), text.as_bytes());
    f.apply_prepared(outcome.prepared.as_ref().unwrap());
    let after = f.artifact();
    assert_eq!(after.raw_response.as_bytes(), f.raw);
    assert_eq!(after.response_hash, before.response_hash);
    assert_eq!(after.allocations, before.allocations);
    assert_eq!(after.mention_spans, before.mention_spans);
    assert_eq!(after.evidence_spans, before.evidence_spans);
    let edited = fs::read_to_string(&path).unwrap();
    assert!(edited.contains("custom_author: \"retain me\"\n# author comment\n"));
    assert!(edited.ends_with("\nUser explanation stays byte-exact.\n"));
    for id in after.allocations.assertions.values() {
        let note = parse_note(
            &fs::read(f.root.path().join(record_path("assertions", id).as_str())).unwrap(),
        );
        assert_eq!(
            note.canonical.unwrap().string("wiki_status"),
            Some("proposed")
        );
    }
    let evidence = &after.allocations.evidence[&local("a1")][0];
    let raw = fs::read(
        f.root
            .path()
            .join(record_path("evidence", evidence).as_str()),
    )
    .unwrap();
    assert!(
        raw.windows(b"Ada works for Acme.\r\n\n".len())
            .any(|w| w == b"Ada works for Acme.\r\n\n")
    );
}

#[test]
fn strict_wire_bounds_duplicate_null_unknown_and_remapping_reject() {
    let f = Fixture::new();
    let base = f.request(vec![create("m1", "Ada", "person")]);
    let variants: Vec<Value> = vec![
        {
            let mut v = base.clone();
            v["extra"] = json!(true);
            v
        },
        {
            let mut v = base.clone();
            v["mappings"][0]["extra"] = json!(true);
            v
        },
        {
            let mut v = base.clone();
            v["mappings"][0]["entity_id"] = json!("entity_guessed");
            v
        },
        {
            let mut v = base.clone();
            v["mappings"][0]["title"] = Value::Null;
            v
        },
        {
            let mut v = base.clone();
            v["mappings"][0]["reason"] = json!("");
            v
        },
        {
            let mut v = base.clone();
            v["mappings"][0]["reason"] = json!("x".repeat(4097));
            v
        },
        {
            let mut v = base.clone();
            v["mappings"][0]["title"] = json!("界".repeat(342));
            v
        },
        {
            let mut v = base.clone();
            v["mappings"][0]["entity_type"] = json!("imaginary");
            v
        },
        {
            let mut v = base.clone();
            v["mappings"][0]["mention_id"] = json!("absent");
            v
        },
        {
            let mut v = base.clone();
            v["mappings"] = json!([create("m1", "Ada", "person"), reject("m1")]);
            v
        },
        {
            let mut v = base.clone();
            v["mappings"] = json!([]);
            v
        },
        {
            let mut v = base.clone();
            v["mappings"] = json!(vec![create("m1", "Ada", "person"); 65]);
            v
        },
        {
            let mut v = base.clone();
            v["mappings"][0]["operation"] = json!("bind_mention");
            v
        },
        {
            let mut v = base.clone();
            v["mappings"][0]["accepted"] = json!(true);
            v
        },
    ];
    for (index, value) in variants.iter().enumerate() {
        assert!(
            validate_resolution(&f.view(), &serde_json::to_vec(value).unwrap()).is_err(),
            "case {index}"
        );
    }
    let raw = serde_json::to_string(&base).unwrap();
    let duplicate = raw.replacen(
        "\"schema\":",
        "\"schema\":\"lwiki.graph-resolution.v1\",\"schema\":",
        1,
    );
    assert!(validate_resolution(&f.view(), duplicate.as_bytes()).is_err());
    assert!(validate_resolution(&f.view(), &vec![b' '; MAX_RESOLUTION_BYTES + 1]).is_err());
    f.resolve(&base);
    assert!(
        validate_resolution(
            &f.view(),
            &serde_json::to_vec(&f.request(vec![reject("m1")])).unwrap()
        )
        .is_err()
    );
}

#[test]
fn staged_and_applied_retries_allocate_once_and_same_base_conflicts() {
    let f = Fixture::new();
    let request = f.request(vec![
        create("m1", "Ada", "person"),
        create("m2", "Acme", "organization"),
    ]);
    let first = f.stage(&request);
    let mut reversed = request.clone();
    reversed["mappings"].as_array_mut().unwrap().reverse();
    let second = f.stage(&reversed);
    assert!(second.reused);
    assert_eq!(first.prepared, second.prepared);
    assert_eq!(first.allocations, second.allocations);
    let mut conflicting = request.clone();
    conflicting["mappings"][0]["title"] = json!("Distinct answer");
    let error = stage_resolution(&f.engine, &f.writer(), &f.validate(&conflicting)).unwrap_err();
    assert_eq!(error.code, ErrorCode::ContentConflict);
    f.apply_prepared(first.prepared.as_ref().unwrap());
    let after = f.stage(&request);
    assert!(after.reused);
    assert_eq!(after.prepared, first.prepared);
    assert_eq!(after.status, Some(ChangeStatus::Committed));
    assert_eq!(after.allocations, first.allocations);
}

#[test]
fn explicit_shared_entity_mentions_commute_and_partial_batches_materialize_once() {
    let f = Fixture::new();
    let entity = f.existing();
    let first = f.request(vec![f.bind("m1", &entity), f.bind("m3", &entity)]);
    let outcome = f.resolve(&first);
    assert_eq!(outcome.summary.materialize_assertions, [local("a4")]);
    let artifact = f.artifact();
    assert_eq!(artifact.bindings.len(), 5);
    let second = f.request(vec![
        create("m2", "Acme", "organization"),
        create("m5", "Tool", "component"),
    ]);
    let second_out = f.resolve(&second);
    assert_eq!(
        second_out.summary.materialize_assertions,
        [local("a1"), local("a2")]
    );
    let previous = artifact.allocations;
    let third = f.request(vec![f.bind("m4", &entity)]);
    f.resolve(&third);
    assert_eq!(f.artifact().allocations, previous);
    assert_eq!(f.artifact().materialized_assertions.len(), 4);
    let restored = load_resolution_receipt(&f.view(), f.validate(&first).task_id())
        .unwrap()
        .unwrap();
    assert_eq!(restored.receipt().allocations, outcome.allocations);
    let reimport = f.stage(&first);
    assert!(reimport.reused);
    assert_eq!(reimport.allocations, outcome.allocations);
    assert_eq!(f.artifact().materialized_assertions.len(), 4);
}

#[test]
fn source_entity_and_extraction_drift_conflict_before_stage_and_apply() {
    for phase in ["before-stage", "before-apply"] {
        for target in ["entity", "source", "extraction"] {
            let f = Fixture::new();
            let request = f.request(vec![
                f.bind("m1", &f.existing()),
                create("m2", "Acme", "organization"),
            ]);
            let validated = f.validate(&request);
            let prepared = if phase == "before-apply" {
                Some(
                    stage_resolution(&f.engine, &f.writer(), &validated)
                        .unwrap()
                        .prepared
                        .unwrap(),
                )
            } else {
                None
            };
            let path = match target {
                "entity" => f.entity_note(&f.existing()).0,
                "source" => f
                    .root
                    .scan_markdown()
                    .unwrap()
                    .into_iter()
                    .find(|p| {
                        parse_note(&fs::read(f.root.path().join(p.as_str())).unwrap())
                            .canonical
                            .is_some_and(|r| r.id() == &f.source)
                    })
                    .unwrap(),
                _ => load_extraction(&f.view(), &f.extraction)
                    .unwrap()
                    .locator()
                    .path
                    .clone(),
            };
            let absolute = f.root.path().join(path.as_str());
            let mut bytes = fs::read(&absolute).unwrap();
            bytes.extend_from_slice(b"\nExternal editor changed authorizing bytes.\n");
            fs::write(&absolute, bytes).unwrap();
            let error = if let Some(prepared) = prepared {
                f.engine
                    .apply(&f.writer(), &prepared, &CatalogGraphValidator, &f.catalog)
                    .unwrap_err()
            } else {
                stage_resolution(&f.engine, &f.writer(), &validated).unwrap_err()
            };
            assert_eq!(error.code, ErrorCode::ContentConflict, "{phase}/{target}");
            assert!(f.artifact().materialized_assertions.is_empty());
        }
    }
}

fn snapshot(root: &Path) -> BTreeMap<String, (Vec<u8>, std::time::SystemTime)> {
    fn walk(
        root: &Path,
        path: &Path,
        out: &mut BTreeMap<String, (Vec<u8>, std::time::SystemTime)>,
    ) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, out)
            } else {
                out.insert(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .to_string(),
                    (
                        fs::read(&path).unwrap(),
                        fs::metadata(&path).unwrap().modified().unwrap(),
                    ),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}
#[test]
fn pure_plan_dry_run_preserves_membership_bytes_and_mtime() {
    let f = Fixture::new();
    let request = f.request(creates());
    let before = snapshot(f.root.path());
    let validated = f.validate(&request);
    let plan = plan_resolution(&validated).unwrap();
    assert_eq!(plan.summary.create_entities, 5);
    assert_eq!(plan.summary.create_decisions, 5);
    assert_eq!(plan.summary.materialize_assertions.len(), 4);
    assert_eq!(snapshot(f.root.path()), before);
}

#[test]
fn canonical_only_restore_keeps_ids_user_entity_title_and_review_status() {
    let f = Fixture::new();
    let request = f.request(creates());
    let resolved = f.resolve(&request);
    let original = f.artifact();
    let assertion = &original.allocations.assertions[&local("a1")];
    let evidence = &original.allocations.evidence[&local("a1")][0];
    f.edit_status(record_path("assertions", assertion), "accepted");
    f.edit_status(record_path("assertions", assertion), "rejected");
    let packet = load_packet(&f.view(), &original.packet_id).unwrap();
    let content = format!(
        "{}\r\nSuccessor context\r\n",
        packet.packet().windows[0].text
    );
    let refresh = SourceStore::new(f.engine.fs().clone())
        .plan_refresh(
            &f.source,
            CaptureRequest {
                title: "Resolution source".into(),
                origin_kind: SourceOrigin::LocalFile,
                origin: "disposable.md".into(),
                original: content.into_bytes(),
                extraction: ExtractionInput::Utf8Preserve,
                media_type: Some("text/markdown".into()),
            },
        )
        .unwrap();
    let next_revision = refresh.revision_id;
    f.apply(refresh.draft.unwrap());
    let evidence_note = parse_note(
        &fs::read(
            f.root
                .path()
                .join(record_path("evidence", evidence).as_str()),
        )
        .unwrap(),
    );
    let successor = SourceStore::new(f.engine.fs().clone())
        .plan_revalidate(evidence, &next_revision, &evidence_note.source_hash)
        .unwrap();
    assert_ne!(successor.evidence_id, *evidence);
    f.apply(successor.draft);
    f.edit_status(record_path("evidence", evidence), "retracted");
    let entity = &resolved.allocations.entities[&local("m1")];
    let (path, note) = f.entity_note(entity);
    let bytes = edit_note(
        &note,
        &BTreeMap::from([("title".into(), json!("Ada explicitly renamed"))]),
        None,
        &note.source_hash,
    )
    .unwrap();
    fs::write(f.root.path().join(path.as_str()), bytes).unwrap();
    fs::remove_dir_all(f.root.path().join(".wiki")).unwrap();
    fs::remove_dir_all(f.root.path().join("changes")).unwrap();
    let before = snapshot(f.root.path());
    let validated = f.validate(&request);
    let outcome = stage_resolution(&f.engine, &f.writer(), &validated).unwrap();
    assert!(outcome.reused);
    assert_eq!(
        outcome.disposition,
        ResolutionDisposition::CanonicalRestored
    );
    assert!(outcome.prepared.is_none());
    assert!(outcome.status.is_none());
    assert_eq!(outcome.allocations, resolved.allocations);
    assert_eq!(f.artifact(), original);
    // The writer can create its operational lock directories, but canonical bytes stay intact.
    for (path, (bytes, _)) in before {
        assert_eq!(fs::read(f.root.path().join(path)).unwrap(), bytes);
    }
    assert_eq!(
        f.entity_note(entity).1.canonical.unwrap().string("title"),
        Some("Ada explicitly renamed")
    );
    let note = parse_note(
        &fs::read(
            f.root
                .path()
                .join(record_path("assertions", assertion).as_str()),
        )
        .unwrap(),
    );
    assert_eq!(
        note.canonical.unwrap().string("wiki_status"),
        Some("rejected")
    );
}

fn receipt_edit(f: &Fixture, outcome: &ResolutionOutcome, change: &dyn Fn(&mut Value)) {
    for decision in outcome.allocations.decisions.values() {
        let path = f
            .root
            .path()
            .join(record_path("decisions", decision).as_str());
        let raw = fs::read_to_string(&path).unwrap();
        let open = format!("```{RESOLUTION_FENCE}\n");
        let start = raw.find(&open).unwrap() + open.len();
        let end = start + raw[start..].find("\n```").unwrap();
        let mut value: Value = serde_json::from_str(&raw[start..end]).unwrap();
        change(&mut value);
        let mut edited = raw[..start].to_string();
        edited.push_str(&String::from_utf8(canonical_json(&value).unwrap()).unwrap());
        edited.push_str(&raw[end..]);
        fs::write(path, edited).unwrap();
    }
}
#[test]
fn restoration_rejects_tampered_receipts_maps_operation_proofs_and_fences() {
    type Mutation = Box<dyn Fn(&mut Value)>;
    let mutations: Vec<Mutation> = vec![
        Box::new(|v| v["extra"] = json!(true)),
        Box::new(|v| v["request"]["mappings"][0]["reason"] = Value::Null),
        Box::new(|v| v["allocations"]["entities"]["m1"] = json!("entity_other")),
        Box::new(|v| {
            v["allocations"]["decisions"]
                .as_object_mut()
                .unwrap()
                .remove("m1")
                .map(|_| ())
                .unwrap()
        }),
        Box::new(|v| {
            v["transitions"]["m1"]["before"] =
                json!({"state":"rejected","decision_id":"decision_fake"})
        }),
        Box::new(|v| v["immutable_extraction_hash"] = json!(Blake3Hash::digest(b"wrong"))),
        Box::new(|v| {
            v["prior_bindings"].as_object_mut().unwrap().remove("m1");
        }),
        Box::new(|v| v["prior_materialized_assertions"] = json!(["a1"])),
        Box::new(|v| {
            let map = v["record_paths"].as_object_mut().unwrap();
            let key = map.keys().next().unwrap().clone();
            map.remove(&key);
        }),
        Box::new(|v| v["operations"][0]["after"]["hash"] = json!(Blake3Hash::digest(b"wrong"))),
        Box::new(|v| {
            v["operations"].as_array_mut().unwrap().pop();
        }),
        Box::new(|v| v["materialized_assertions"] = json!([])),
    ];
    for (index, change) in mutations.iter().enumerate() {
        let f = Fixture::new();
        let request = f.request(creates());
        let outcome = f.resolve(&request);
        receipt_edit(&f, &outcome, change.as_ref());
        assert!(
            validate_resolution(&f.view(), &serde_json::to_vec(&request).unwrap()).is_err(),
            "receipt case {index}"
        );
    }
    let f = Fixture::new();
    let request = f.request(creates());
    let outcome = f.resolve(&request);
    let decision = outcome.allocations.decisions.values().next().unwrap();
    let path = f
        .root
        .path()
        .join(record_path("decisions", decision).as_str());
    let mut text = fs::read_to_string(&path).unwrap();
    let artifact = text
        .split_once(&format!("```{RESOLUTION_FENCE}\n"))
        .unwrap()
        .1
        .split_once("\n```")
        .unwrap()
        .0
        .to_string();
    text.push_str(&format!("\n```{RESOLUTION_FENCE}\n{artifact}\n```\n"));
    fs::write(path, text).unwrap();
    assert!(validate_resolution(&f.view(), &serde_json::to_vec(&request).unwrap()).is_err());
}

#[test]
fn selected_source_payload_cap_refuses_before_allocation() {
    let f = Fixture::new();
    let request = f.request(creates());
    let path = f
        .root
        .scan_markdown()
        .unwrap()
        .into_iter()
        .find(|p| {
            parse_note(&fs::read(f.root.path().join(p.as_str())).unwrap())
                .canonical
                .is_some_and(|r| {
                    r.kind() == RecordKind::Revision
                        && r.string("wiki_source_id") == Some(f.source.as_str())
                })
        })
        .unwrap();
    let note = parse_note(&fs::read(f.root.path().join(path.as_str())).unwrap());
    let payload = note
        .canonical
        .unwrap()
        .string("wiki_original_path")
        .unwrap()
        .to_string();
    let parent = f
        .root
        .path()
        .join(path.as_str())
        .parent()
        .unwrap()
        .to_path_buf();
    let file = fs::OpenOptions::new()
        .write(true)
        .open(parent.join(payload))
        .unwrap();
    file.set_len(64 * 1024 * 1024 + 1).unwrap();
    let before = fs::read_dir(f.root.path().join("changes")).unwrap().count();
    let error = validate_resolution(&f.view(), &serde_json::to_vec(&request).unwrap()).unwrap_err();
    assert_eq!(error.code, ErrorCode::BudgetExceeded);
    assert_eq!(
        fs::read_dir(f.root.path().join("changes")).unwrap().count(),
        before
    );
}

#[test]
fn canonical_batch_b_cannot_pair_with_retained_batch_a_allocations() {
    let f = Fixture::new();
    let request = f.request(creates());
    let a = f.stage(&request);
    let saved = tempfile::tempdir().unwrap();
    fs::rename(f.root.path().join("changes"), saved.path().join("changes")).unwrap();
    let b = f.stage(&request);
    assert_ne!(a.allocations, b.allocations);
    f.apply_prepared(b.prepared.as_ref().unwrap());
    fs::remove_dir_all(f.root.path().join("changes")).unwrap();
    fs::rename(saved.path().join("changes"), f.root.path().join("changes")).unwrap();
    let before = f.artifact();
    let error = stage_resolution(&f.engine, &f.writer(), &f.validate(&request)).unwrap_err();
    assert!(error.message.contains("allocations"));
    assert_eq!(f.artifact(), before);
    assert_eq!(
        f.engine
            .inspect(&a.prepared.unwrap().change_id)
            .unwrap()
            .status,
        ChangeStatus::Prepared
    );
}

#[test]
fn catalog_apply_and_rebuild_refuse_forged_or_incomplete_mention_maps() {
    for invented in [true, false] {
        let f = Fixture::new();
        let loaded = load_extraction(&f.view(), &f.extraction).unwrap();
        let mut artifact = loaded.artifact().clone();
        let decision = id("decision_forged_membership");
        let mention = local(if invented { "m_fake" } else { "m1" });
        artifact.bindings.insert(
            mention.clone(),
            MentionBinding::Resolved {
                entity_id: f.existing(),
                decision_id: decision.clone(),
            },
        );
        if invented {
            // The forged decision and span agree with the invented binding, but
            // the immutable source-local raw response never declared this ID.
            artifact.mention_spans.insert(
                mention.clone(),
                artifact.mention_spans[&local("m1")].clone(),
            );
        } else {
            // The decision's own m1 outcome matches exactly. A different declared
            // mention is missing, so a membership-only decision check is insufficient.
            artifact.bindings.remove(&local("m2"));
        }
        let extraction_path = loaded.locator().path.clone();
        let original = fs::read(f.root.path().join(extraction_path.as_str())).unwrap();
        let text = std::str::from_utf8(&original).unwrap();
        let open = format!("```{ARTIFACT_FENCE}\n");
        let start = text.find(&open).unwrap() + open.len();
        let end = start + text[start..].find("\n```").unwrap();
        let mut forged = text.as_bytes()[..start].to_vec();
        forged.extend(canonical_json(&artifact).unwrap());
        forged.extend_from_slice(&text.as_bytes()[end..]);
        let fields = json!({
            "wiki_schema":"1", "wiki_id":decision, "wiki_kind":"decision",
            "title":"Forged matching mention decision", "wiki_status":"active",
            "wiki_action":"bind_mention", "wiki_created_at":"2026-09-28T00:00:00Z",
            "wiki_extraction_id":f.extraction, "wiki_mention_ids":[mention],
            "wiki_input_ids":[f.extraction], "wiki_output_ids":[f.existing()]
        });
        let mut decision_bytes = b"---\n".to_vec();
        for (key, value) in fields.as_object().unwrap() {
            decision_bytes.extend_from_slice(key.as_bytes());
            decision_bytes.extend_from_slice(b": ");
            decision_bytes.extend(serde_json::to_vec(value).unwrap());
            decision_bytes.push(b'\n');
        }
        decision_bytes.extend_from_slice(b"---\n\nA matching explicit decision cannot authorize an undeclared or incomplete map.\n");
        assert!(parse_note(&decision_bytes).canonical.is_some());
        let decision_path = record_path("decisions", &decision);
        let draft = ChangeDraft {
            title: "Forged membership fixture".into(),
            origin: None,
            inverse_of: None,
            allocated_ids: BTreeMap::new(),
            read_preconditions: vec![],
            operations: vec![
                ExpectedWrite {
                    target: extraction_path.clone(),
                    expected: ExpectedState::Hash(loaded.locator().observed_hash.clone()),
                    proposed: Some(forged.clone()),
                    apply_after: vec![],
                },
                ExpectedWrite {
                    target: decision_path.clone(),
                    expected: ExpectedState::Absent,
                    proposed: Some(decision_bytes.clone()),
                    apply_after: vec![],
                },
            ],
        };
        let prepared = f.engine.prepare(&f.writer(), draft).unwrap().prepared;
        let error = f
            .engine
            .apply(&f.writer(), &prepared, &CatalogGraphValidator, &f.catalog)
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::RecordInvalid);
        assert_eq!(
            fs::read(f.root.path().join(extraction_path.as_str())).unwrap(),
            original
        );
        assert!(!f.root.path().join(decision_path.as_str()).exists());
        f.engine.abort(&f.writer(), &prepared).unwrap();
        // A user can author invalid Markdown directly. Rebuild must retain it as
        // readable diagnostics, with no canonical decision/binding authority.
        fs::write(f.root.path().join(extraction_path.as_str()), forged).unwrap();
        fs::create_dir_all(f.root.path().join("knowledge/decisions")).unwrap();
        fs::write(f.root.path().join(decision_path.as_str()), decision_bytes).unwrap();
        f.catalog.rebuild(&f.writer()).unwrap();
        let snapshot = f.catalog.index_snapshot().unwrap();
        for record in [&f.extraction, &decision] {
            let row = &snapshot.projection().records[record];
            assert_eq!(row.eligibility, Eligibility::Invalid);
            assert!(
                row.reasons
                    .iter()
                    .any(|r| r == "decision_mention_mapping_disagreement"
                        || r == "decision_outcome_disagreement")
            );
        }
        assert!(load_extraction(&f.view(), &f.extraction).is_err());
    }
}

#[test]
fn authorized_entity_extraction_renames_and_author_prose_preserve_resolution_acknowledgement() {
    use lwiki::app::{OfflineApp, OperationOptions};
    let f = Fixture::new();
    let request = f.request(creates());
    let outcome = f.resolve(&request);
    let artifact = f.artifact();
    let entity = &outcome.allocations.entities[&local("m1")];
    let app = OfflineApp::new(
        f.engine.fs().clone(),
        OperationOptions {
            offline: true,
            ..OperationOptions::default()
        },
    )
    .unwrap();
    let (old_entity, note) = f.entity_note(entity);
    let entity_path = VaultRelativePath::new("knowledge/entities/renamed-ada.md").unwrap();
    let renamed = app
        .page_rename(entity.clone(), entity_path.clone(), note.source_hash)
        .unwrap();
    assert_eq!(renamed.status, Some(ChangeStatus::Committed));
    let loaded = load_extraction(&f.view(), &f.extraction).unwrap();
    let old_extraction = loaded.locator().path.clone();
    let extraction_path =
        VaultRelativePath::new("knowledge/extractions/renamed-extraction.md").unwrap();
    let renamed = app
        .page_rename(
            f.extraction.clone(),
            extraction_path.clone(),
            loaded.locator().observed_hash.clone(),
        )
        .unwrap();
    assert_eq!(renamed.status, Some(ChangeStatus::Committed));
    assert!(!f.root.path().join(old_entity.as_str()).exists());
    assert!(!f.root.path().join(old_extraction.as_str()).exists());
    for (path, title) in [
        (entity_path.clone(), "Ada author title"),
        (extraction_path.clone(), "Extraction author title"),
    ] {
        // An author extension is introduced through a guarded raw edit; the
        // record edit API subsequently preserves it while editing known fields.
        let original = fs::read(f.root.path().join(path.as_str())).unwrap();
        let original_note = parse_note(&original);
        assert!(original.starts_with(b"---\n"));
        let mut extended = b"---\nauthor_note: preserve this extension\n".to_vec();
        extended.extend_from_slice(&original[4..]);
        f.apply(ChangeDraft {
            title: "Guarded author extension fixture edit".into(),
            origin: None,
            inverse_of: None,
            allocated_ids: BTreeMap::new(),
            read_preconditions: vec![],
            operations: vec![ExpectedWrite {
                target: path.clone(),
                expected: ExpectedState::Hash(original_note.source_hash),
                proposed: Some(extended),
                apply_after: vec![],
            }],
        });
        let note = parse_note(&fs::read(f.root.path().join(path.as_str())).unwrap());
        let mut body = note.body().to_vec();
        body.extend_from_slice(b"\nAuthor prose retained after identity-preserving rename.\n");
        let proposed = edit_note(
            &note,
            &BTreeMap::from([("title".into(), json!(title))]),
            Some(&body),
            &note.source_hash,
        )
        .unwrap();
        f.apply(ChangeDraft {
            title: "Authorized author metadata/prose fixture edit".into(),
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
    let retained = f.stage(&request);
    assert!(retained.reused);
    assert_eq!(retained.prepared, outcome.prepared);
    assert_eq!(retained.extraction.path, extraction_path);
    assert_eq!(retained.allocations, outcome.allocations);
    fs::remove_dir_all(f.root.path().join(".wiki")).unwrap();
    fs::remove_dir_all(f.root.path().join("changes")).unwrap();
    let before = snapshot(f.root.path());
    let restored = f.stage(&request);
    assert!(restored.prepared.is_none());
    assert!(restored.status.is_none());
    assert_eq!(
        restored.disposition,
        ResolutionDisposition::CanonicalRestored
    );
    assert_eq!(restored.allocations, outcome.allocations);
    assert_eq!(restored.extraction.path, extraction_path);
    assert_eq!(f.artifact(), artifact);
    for (path, (bytes, _)) in before {
        assert_eq!(fs::read(f.root.path().join(path)).unwrap(), bytes);
    }
    let note = parse_note(&fs::read(f.root.path().join(extraction_path.as_str())).unwrap());
    assert_eq!(
        note.canonical.as_ref().unwrap().string("title"),
        Some("Extraction author title")
    );
    assert!(
        note.body()
            .ends_with(b"\nAuthor prose retained after identity-preserving rename.\n")
    );
}

#[test]
fn unrelated_decision_rationale_is_not_a_receipt_and_relevant_malformed_fences_refuse() {
    let f = Fixture::new();
    let request = f.request(creates());
    let outcome = f.resolve(&request);
    let entity = f.existing();
    let fields = json!({"wiki_schema":"1","wiki_id":"decision_ordinary_rationale","wiki_kind":"decision","title":"Ordinary alias rationale","wiki_status":"active","wiki_action":"add_alias","wiki_created_at":"2026-09-28T00:00:00Z","wiki_input_ids":[entity],"wiki_output_ids":[entity]});
    let mut bytes = b"---\n".to_vec();
    for (key, value) in fields.as_object().unwrap() {
        bytes.extend_from_slice(key.as_bytes());
        bytes.extend_from_slice(b": ");
        bytes.extend(serde_json::to_vec(value).unwrap());
        bytes.push(b'\n');
    }
    bytes.extend_from_slice(format!("---\n\nThe name {RESOLUTION_FENCE} is ordinary prose, with no supported receipt fence.\n").as_bytes());
    f.apply(ChangeDraft {
        title: "Ordinary unrelated explicit decision fixture".into(),
        origin: None,
        inverse_of: None,
        allocated_ids: BTreeMap::new(),
        read_preconditions: vec![],
        operations: vec![ExpectedWrite {
            target: record_path("decisions", &id("decision_ordinary_rationale")),
            expected: ExpectedState::Absent,
            proposed: Some(bytes),
            apply_after: vec![],
        }],
    });
    let reused = f.stage(&request);
    assert!(reused.reused);
    assert_eq!(reused.allocations, outcome.allocations);
    let decision = outcome.allocations.decisions.values().next().unwrap();
    let path = f
        .root
        .path()
        .join(record_path("decisions", decision).as_str());
    let text = fs::read_to_string(&path).unwrap();
    let closing = text.rfind("\n```").unwrap();
    fs::write(path, &text[..closing]).unwrap();
    assert!(validate_resolution(&f.view(), &serde_json::to_vec(&request).unwrap()).is_err());
}

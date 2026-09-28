use lwiki::{
    catalog::*,
    changes::*,
    domain::*,
    graph::{extraction_types::*, import::*, packet::*, wire::*},
    sources::*,
    vault::*,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path, time::Duration};
const VAULT: &str = "vault_00000000-0000-7000-8000-00000000001b";
type JsonMutation = Box<dyn Fn(&mut Value)>;
type ArtifactMutation = Box<dyn Fn(&mut ExtractionArtifactV1)>;
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
    temp: tempfile::TempDir,
    root: VaultRoot,
    engine: ChangeEngine,
    catalog: Catalog,
    source: RecordId,
    revision: RecordId,
}
impl Fixture {
    fn new(content: &[u8]) -> Self {
        let temp = tempfile::tempdir().unwrap();
        copy(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bootstrap/vault"),
            temp.path(),
        );
        let root = VaultRoot::explicit(temp.path()).unwrap();
        let vault = VaultFs::new(root.clone());
        let engine = ChangeEngine::new(vault.clone()).unwrap();
        let catalog = Catalog::new(vault.clone(), id(VAULT));
        let plan = SourceStore::new(vault)
            .plan_capture(CaptureRequest {
                title: "Packet test source".into(),
                origin_kind: SourceOrigin::LocalFile,
                origin: "synthetic.md".into(),
                original: content.to_vec(),
                extraction: ExtractionInput::Utf8Preserve,
                media_type: Some("text/markdown".into()),
            })
            .unwrap();
        let source = plan.source_id;
        let revision = plan.revision_id;
        let fixture = Self {
            temp,
            root,
            engine,
            catalog,
            source,
            revision,
        };
        fixture.apply(plan.draft.unwrap());
        fixture
    }
    fn writer(&self) -> WriterPermit {
        WriterPermit::acquire(&self.root, Duration::from_secs(2)).unwrap()
    }
    fn view(&self) -> SourceView<'_> {
        SourceView::from_fs_bounded(self.engine.fs(), 64 * 1024 * 1024, 4096).unwrap()
    }
    fn apply(&self, draft: ChangeDraft) -> PreparedChange {
        let writer = self.writer();
        let prepared = self.engine.prepare(&writer, draft).unwrap().prepared;
        self.engine
            .apply(&writer, &prepared, &CatalogGraphValidator, &self.catalog)
            .unwrap();
        prepared
    }
    fn request(&self) -> ExportRequest {
        ExportRequest {
            source_id: self.source.clone(),
            revision_id: Some(self.revision.clone()),
            windows: vec![],
            limits: ExtractionLimits::default(),
            candidate_context: vec![],
        }
    }
    fn packet(&self) -> VerifiedPacket {
        let plan = build_packet(&self.view(), &self.request()).unwrap();
        if let Some(draft) = plan.draft {
            self.apply(draft);
        }
        load_packet(&self.view(), &plan.packet.packet_id).unwrap()
    }
    fn response(&self, packet: &VerifiedPacket) -> Value {
        json!({"schema":EXTRACTION_SCHEMA,"packet_id":packet.packet().packet_id,"packet_fingerprint":packet.packet().packet_fingerprint,"mentions":[{"id":"m1","window_id":"w1","label":"Ada","type":"person","quote":"Ada","span":{"start":8,"end":11}},{"id":"m2","window_id":"w1","label":"Acme","type":"organization","quote":"Acme","span":{"start":22,"end":26}}],"assertions":[{"id":"a1","subject":"m1","predicate":"works_for","object":{"kind":"mention","mention_id":"m2"},"negated":false,"modality":"asserted","evidence":[{"window_id":"w1","stance":"supports","quote":"Ada works for Acme."}]}],"unresolved":[]})
    }
    fn stage(
        &self,
        packet: &VerifiedPacket,
        response: &Value,
        policy: OriginPolicy,
    ) -> ImportOutcome {
        let validated =
            validate_response(packet, &self.view(), &serde_json::to_vec(response).unwrap())
                .unwrap();
        stage_import(&self.engine, &self.writer(), &validated, policy).unwrap()
    }
    fn apply_import(&self, outcome: &ImportOutcome) {
        self.engine
            .apply(
                &self.writer(),
                outcome.prepared.as_ref().unwrap(),
                &CatalogGraphValidator,
                &self.catalog,
            )
            .unwrap();
    }
}
fn text() -> Vec<u8> {
    include_bytes!("fixtures/p10/source.md").to_vec()
}
#[test]
fn packet_exact_window_fingerprint_and_markdown_restore() {
    let fixture = Fixture::new(&text());
    let request = fixture.request();
    let first = build_packet(&fixture.view(), &request).unwrap();
    assert!(!first.reused);
    assert_eq!(
        first.packet.windows[0].text,
        std::str::from_utf8(&text()).unwrap()
    );
    assert_eq!(
        first.packet.packet_id,
        RecordId::packet(&first.packet.packet_fingerprint)
    );
    assert_eq!(
        packet_fingerprint(&first.packet).unwrap(),
        first.packet.packet_fingerprint
    );
    let again = build_packet(&fixture.view(), &request).unwrap();
    assert_eq!(
        again.packet.packet_fingerprint,
        first.packet.packet_fingerprint
    );
    fixture.apply(first.draft.unwrap());
    let persisted = build_packet(&fixture.view(), &request).unwrap();
    assert!(persisted.reused);
    assert!(persisted.draft.is_none());
    let hash = persisted.locator.observed_hash.clone();
    fs::remove_dir_all(fixture.root.path().join(".wiki")).unwrap();
    let restored = load_packet(&fixture.view(), &persisted.packet.packet_id).unwrap();
    assert_eq!(restored.locator().observed_hash, hash);
    let mut changed = restored.packet().clone();
    changed.instructions.push_str(" New prompt version.");
    assert_ne!(
        packet_fingerprint(&changed).unwrap(),
        restored.packet().packet_fingerprint
    );
    changed = restored.packet().clone();
    changed.output_schema["description"] = json!("new schema");
    assert_ne!(
        packet_fingerprint(&changed).unwrap(),
        restored.packet().packet_fingerprint
    );
    let mut tiny = request;
    tiny.windows = vec![ByteSpan::new(8, 27).unwrap()];
    let plan = build_packet(&fixture.view(), &tiny).unwrap();
    assert_eq!(plan.coverage.selected_bytes, 19);
    assert!(plan.coverage.omitted_source_bytes > 0);
    assert_eq!(plan.packet.windows[0].headings, ["Team"]);
}
#[test]
fn wire_duplicate_unknown_reference_qualifier_and_quote_rejections() {
    let fixture = Fixture::new(&text());
    let packet = fixture.packet();
    let valid = fixture.response(&packet);
    let changes: Vec<JsonMutation> = vec![
        Box::new(|v| v["extra"] = json!(true)),
        Box::new(|v| v["mentions"][0]["extra"] = json!(true)),
        Box::new(|v| v["assertions"][0]["object"]["extra"] = json!(true)),
        Box::new(|v| v["assertions"][0]["evidence"][0]["span"] = Value::Null),
        Box::new(|v| v["mentions"][0]["description"] = Value::Null),
        Box::new(|v| v["assertions"][0]["unit"] = Value::Null),
        Box::new(|v| v["assertions"][0]["subject"] = json!("missing")),
        Box::new(|v| v["mentions"][1]["id"] = json!("m1")),
        Box::new(|v| v["assertions"][0]["modality"] = json!("certain")),
        Box::new(|v| v["assertions"][0]["valid_from"] = json!("2026-13-02")),
        Box::new(|v| {
            v["assertions"][0]["valid_from"] = json!("2026-10-02");
            v["assertions"][0]["valid_until"] = json!("2026-10-01");
        }),
        Box::new(|v| v["assertions"][0]["evidence"][0]["quote"] = json!("invented quote")),
        Box::new(|v| {
            v["mentions"][0]
                .as_object_mut()
                .unwrap()
                .remove("span")
                .map(|_| ())
                .unwrap()
        }),
        Box::new(|v| v["assertions"][0]["evidence"] = json!([])),
        Box::new(|v| {
            v["assertions"][0]["object"] = json!({"kind":"literal","type":"decimal","value":"2.5"})
        }),
        Box::new(|v| v["schema"] = json!("lwiki.extraction.v99")),
    ];
    for (i, change) in changes.into_iter().enumerate() {
        let mut value = valid.clone();
        change(&mut value);
        assert!(
            validate_response(
                &packet,
                &fixture.view(),
                &serde_json::to_vec(&value).unwrap()
            )
            .is_err(),
            "case {i}"
        );
    }
    let raw = serde_json::to_string(&valid).unwrap();
    let duplicate = raw.replacen("\"id\":\"m1\"", "\"id\":\"m1\",\"id\":\"m1\"", 1);
    assert!(validate_response(&packet, &fixture.view(), duplicate.as_bytes()).is_err());
    assert!(
        !fs::read_dir(fixture.root.path().join("knowledge/extractions"))
            .unwrap()
            .any(|e| e.unwrap().path().is_file())
    );
}
#[test]
fn identical_staged_import_reuses_ids_before_apply() {
    let fixture = Fixture::new(&text());
    let packet = fixture.packet();
    let response = fixture.response(&packet);
    let first = fixture.stage(&packet, &response, OriginPolicy::ReuseOrConflict);
    let second = fixture.stage(&packet, &response, OriginPolicy::ReuseOrConflict);
    assert!(!first.reused);
    assert!(second.reused);
    assert_eq!(first.prepared, second.prepared);
    assert_eq!(first.extraction, second.extraction);
    assert_eq!(first.allocations, second.allocations);
    assert_eq!(first.coverage.pending_mentions, 2);
    assert_eq!(first.coverage.materialized_assertions, 0);
    fixture.apply_import(&first);
    let after = fixture.stage(&packet, &response, OriginPolicy::ReuseOrConflict);
    assert!(after.reused);
    assert_eq!(after.prepared, first.prepared);
    let loaded = load_extraction(
        &fixture.view(),
        &first.extraction.record.as_ref().unwrap().record_id,
    )
    .unwrap();
    assert_eq!(loaded.artifact().allocations, first.allocations);
    assert!(
        loaded
            .artifact()
            .bindings
            .values()
            .all(|b| matches!(b, MentionBinding::Pending))
    );
    for reserved in first
        .allocations
        .assertions
        .values()
        .chain(first.allocations.evidence.values().flatten())
    {
        assert!(!fixture.view_has_id(reserved));
    }
}
impl Fixture {
    fn view_has_id(&self, id: &RecordId) -> bool {
        self.root.scan_markdown().unwrap().iter().any(|path| {
            lwiki::records::parse_note(&fs::read(self.root.path().join(path.as_str())).unwrap())
                .canonical
                .is_some_and(|r| r.id() == id)
        })
    }
}
#[test]
fn conflicting_response_requires_new_extraction() {
    let fixture = Fixture::new(&text());
    let packet = fixture.packet();
    let response = fixture.response(&packet);
    let first = fixture.stage(&packet, &response, OriginPolicy::ReuseOrConflict);
    fixture.apply_import(&first);
    let mut changed = response.clone();
    changed["assertions"][0]["negated"] = json!(true);
    let validated = validate_response(
        &packet,
        &fixture.view(),
        &serde_json::to_vec(&changed).unwrap(),
    )
    .unwrap();
    assert_eq!(
        stage_import(
            &fixture.engine,
            &fixture.writer(),
            &validated,
            OriginPolicy::ReuseOrConflict
        )
        .unwrap_err()
        .code,
        ErrorCode::ContentConflict
    );
    let other = stage_import(
        &fixture.engine,
        &fixture.writer(),
        &validated,
        OriginPolicy::AllowNewResponse,
    )
    .unwrap();
    assert_ne!(
        other.extraction.record.as_ref().unwrap().record_id,
        first.extraction.record.as_ref().unwrap().record_id
    );
    fixture.apply_import(&other);
    assert!(load_extraction(&fixture.view(), &first.extraction.record.unwrap().record_id).is_ok());
}
#[test]
fn model_accepted_flag_cannot_activate() {
    let fixture = Fixture::new(&text());
    let packet = fixture.packet();
    let mut response = fixture.response(&packet);
    response["assertions"][0]["accepted"] = json!(true);
    assert!(
        validate_response(
            &packet,
            &fixture.view(),
            &serde_json::to_vec(&response).unwrap()
        )
        .is_err()
    );
    response["assertions"][0]
        .as_object_mut()
        .unwrap()
        .remove("accepted");
    response["bindings"] = json!({"m1":{"state":"resolved","entity_id":"entity_forged"}});
    assert!(
        validate_response(
            &packet,
            &fixture.view(),
            &serde_json::to_vec(&response).unwrap()
        )
        .is_err()
    );
    let valid = fixture.response(&packet);
    let outcome = fixture.stage(&packet, &valid, OriginPolicy::ReuseOrConflict);
    fixture.apply_import(&outcome);
    assert_eq!(outcome.coverage.materialized_assertions, 0);
    assert_eq!(outcome.coverage.pending_mentions, 2);
    assert_eq!(outcome.allocations.assertions.len(), 1);
}

fn record(fields: Value, body: &[u8]) -> Vec<u8> {
    let mut bytes = b"---\n".to_vec();
    for (key, value) in fields.as_object().unwrap() {
        bytes.extend_from_slice(key.as_bytes());
        bytes.extend_from_slice(b": ");
        bytes.extend(serde_json::to_vec(value).unwrap());
        bytes.push(b'\n');
    }
    bytes.extend_from_slice(b"---\n");
    bytes.extend_from_slice(body);
    assert!(lwiki::records::parse_note(&bytes).canonical.is_some());
    bytes
}
fn artifact_edit(
    fixture: &Fixture,
    outcome: &ImportOutcome,
    artifact: &ExtractionArtifactV1,
) -> ExpectedWrite {
    let target = outcome.extraction.path.clone();
    let bytes = fs::read(fixture.root.path().join(target.as_str())).unwrap();
    let note = lwiki::records::parse_note(&bytes);
    let body = artifact_body(artifact).unwrap();
    let edited =
        lwiki::records::edit_note(&note, &BTreeMap::new(), Some(&body), &note.source_hash).unwrap();
    ExpectedWrite {
        target,
        expected: ExpectedState::Hash(note.source_hash),
        proposed: Some(edited),
        apply_after: vec![],
    }
}
fn explicit_binding(
    fixture: &Fixture,
    outcome: &ImportOutcome,
    resolve_second: bool,
) -> ExtractionArtifactV1 {
    let loaded = load_extraction(
        &fixture.view(),
        &outcome.extraction.record.as_ref().unwrap().record_id,
    )
    .unwrap();
    let mut artifact = loaded.artifact().clone();
    let mut writes = vec![];
    for (local, entity) in [
        ("m1", Some("entity_00000000-0000-7000-8000-000000000001")),
        (
            "m2",
            resolve_second.then_some("entity_00000000-0000-7000-8000-000000000003"),
        ),
    ] {
        let decision = id(&format!("decision_p10_{local}"));
        let action = if entity.is_some() {
            "bind_mention"
        } else {
            "reject_mention"
        };
        let fields = json!({"wiki_schema":"1","wiki_id":decision,"wiki_kind":"decision","title":"Explicit fixture binding","wiki_status":"active","wiki_action":action,"wiki_input_ids":[],"wiki_output_ids":entity.into_iter().collect::<Vec<_>>(),"wiki_created_at":"2026-09-28T00:00:00Z","wiki_extraction_id":artifact.extraction_id,"wiki_mention_ids":[local]});
        writes.push(ExpectedWrite {
            target: VaultRelativePath::new(format!("knowledge/decisions/{decision}.md")).unwrap(),
            expected: ExpectedState::Absent,
            proposed: Some(record(
                fields,
                b"\nExplicit test decision, not a model inference.\n",
            )),
            apply_after: vec![],
        });
        artifact.bindings.insert(
            PacketLocalId::new(local).unwrap(),
            if let Some(entity) = entity {
                MentionBinding::Resolved {
                    entity_id: id(entity),
                    decision_id: decision,
                }
            } else {
                MentionBinding::Rejected {
                    decision_id: decision,
                }
            },
        );
    }
    writes.push(artifact_edit(fixture, outcome, &artifact));
    fixture.apply(ChangeDraft {
        title: "Explicit fixture resolution".into(),
        origin: None,
        inverse_of: None,
        allocated_ids: BTreeMap::new(),
        read_preconditions: vec![],
        operations: writes,
    });
    artifact
}
#[test]
fn canonical_only_restore_reuses_resolved_rejected_maps_and_exact_raw_response() {
    let fixture = Fixture::new(&text());
    let packet = fixture.packet();
    let response = fixture.response(&packet);
    let raw = format!("  {}\n", serde_json::to_string_pretty(&response).unwrap());
    let validated = validate_response(&packet, &fixture.view(), raw.as_bytes()).unwrap();
    let first = stage_import(
        &fixture.engine,
        &fixture.writer(),
        &validated,
        OriginPolicy::ReuseOrConflict,
    )
    .unwrap();
    fixture.apply_import(&first);
    let bound = explicit_binding(&fixture, &first, false);
    let original_hash = first.extraction.observed_hash.clone();
    let existing = stage_import(
        &fixture.engine,
        &fixture.writer(),
        &validated,
        OriginPolicy::ReuseOrConflict,
    )
    .unwrap();
    assert!(existing.reused);
    assert_ne!(existing.extraction.observed_hash, original_hash);
    assert_eq!(existing.coverage.pending_mentions, 0);
    assert_eq!(existing.coverage.rejected_mentions, 1);
    assert_eq!(existing.allocations, first.allocations);
    fs::remove_dir_all(fixture.temp.path().join("changes")).unwrap();
    fs::remove_dir_all(fixture.temp.path().join(".wiki")).unwrap();
    let restored = stage_import(
        &fixture.engine,
        &fixture.writer(),
        &validated,
        OriginPolicy::ReuseOrConflict,
    )
    .unwrap();
    assert!(restored.reused);
    assert_eq!(restored.disposition, ImportDisposition::CanonicalRestored);
    assert!(restored.prepared.is_none());
    assert!(restored.status.is_none());
    assert_eq!(restored.allocations, first.allocations);
    let loaded = load_extraction(
        &fixture.view(),
        &first.extraction.record.as_ref().unwrap().record_id,
    )
    .unwrap();
    assert_eq!(loaded.artifact().bindings, bound.bindings);
    assert_eq!(loaded.artifact().raw_response, raw);
    assert_eq!(
        loaded.artifact().response_hash,
        Blake3Hash::digest(raw.as_bytes())
    );
}
#[test]
fn guarded_import_refuses_changed_authorizing_source_without_allocations() {
    let fixture = Fixture::new(&text());
    let packet = fixture.packet();
    let response = fixture.response(&packet);
    let validated = validate_response(
        &packet,
        &fixture.view(),
        &serde_json::to_vec(&response).unwrap(),
    )
    .unwrap();
    let source_path = fixture
        .root
        .path()
        .join(format!("sources/{}/source.md", fixture.source));
    let original = fs::read(&source_path).unwrap();
    fs::write(
        &source_path,
        [original.clone(), b"\nExternal editor\n".to_vec()].concat(),
    )
    .unwrap();
    assert_eq!(
        stage_import(
            &fixture.engine,
            &fixture.writer(),
            &validated,
            OriginPolicy::ReuseOrConflict
        )
        .unwrap_err()
        .code,
        ErrorCode::ContentConflict
    );
    assert!(
        validate_response(
            &packet,
            &fixture.view(),
            &serde_json::to_vec(&response).unwrap()
        )
        .is_err()
    );
    fs::write(source_path, original).unwrap();
    let outcome = fixture.stage(&packet, &response, OriginPolicy::ReuseOrConflict);
    let source_path = fixture
        .root
        .path()
        .join(format!("sources/{}/source.md", fixture.source));
    let original = fs::read(&source_path).unwrap();
    fs::write(
        source_path,
        [original, b"\nLater editor\n".to_vec()].concat(),
    )
    .unwrap();
    assert!(
        fixture
            .engine
            .apply(
                &fixture.writer(),
                outcome.prepared.as_ref().unwrap(),
                &CatalogGraphValidator,
                &fixture.catalog
            )
            .is_err()
    );
    assert!(
        !fixture
            .root
            .path()
            .join(outcome.extraction.path.as_str())
            .exists()
    );
}
#[test]
fn artifact_raw_proof_map_and_decision_tampering_is_refused() {
    let fixture = Fixture::new(&text());
    let packet = fixture.packet();
    let outcome = fixture.stage(
        &packet,
        &fixture.response(&packet),
        OriginPolicy::ReuseOrConflict,
    );
    fixture.apply_import(&outcome);
    let extraction_id = outcome
        .extraction
        .record
        .as_ref()
        .unwrap()
        .record_id
        .clone();
    let loaded = load_extraction(&fixture.view(), &extraction_id).unwrap();
    let artifact = loaded.artifact().clone();
    let path = fixture.root.path().join(outcome.extraction.path.as_str());
    let original = fs::read(&path).unwrap();
    let edits: Vec<ArtifactMutation> = vec![
        Box::new(|a| a.raw_response.push(' ')),
        Box::new(|a| {
            a.mention_spans.get_mut(&local("m1")).unwrap().quote_hash = Blake3Hash::digest(b"wrong")
        }),
        Box::new(|a| {
            a.bindings.remove(&local("m2"));
        }),
        Box::new(|a| {
            a.allocations
                .evidence
                .get_mut(&local("a1"))
                .unwrap()
                .clear();
        }),
        Box::new(|a| {
            a.allocations
                .assertions
                .insert(local("a1"), a.extraction_id.clone())
                .map(|_| ())
                .unwrap()
        }),
        Box::new(|a| a.materialized_assertions.push(local("a1"))),
        Box::new(|a| {
            a.bindings.insert(
                local("m1"),
                MentionBinding::Resolved {
                    entity_id: id("entity_00000000-0000-7000-8000-000000000001"),
                    decision_id: id("decision_missing"),
                },
            );
        }),
    ];
    for edit in edits {
        let mut corrupted = artifact.clone();
        edit(&mut corrupted);
        let write = artifact_edit(&fixture, &outcome, &corrupted);
        fs::write(&path, write.proposed.unwrap()).unwrap();
        assert!(load_extraction(&fixture.view(), &extraction_id).is_err());
        fs::write(&path, &original).unwrap();
    }
    let note = lwiki::records::parse_note(&original);
    let mut body = artifact_body(&artifact).unwrap();
    body.extend(artifact_body(&artifact).unwrap());
    let bytes =
        lwiki::records::edit_note(&note, &BTreeMap::new(), Some(&body), &note.source_hash).unwrap();
    fs::write(&path, bytes).unwrap();
    assert!(load_extraction(&fixture.view(), &extraction_id).is_err());
    let fake = format!(
        "\n<!--\n```{ARTIFACT_FENCE}\n{}\n```\n-->\n",
        serde_json::to_string(&artifact).unwrap()
    );
    let bytes = lwiki::records::edit_note(
        &note,
        &BTreeMap::new(),
        Some(fake.as_bytes()),
        &note.source_hash,
    )
    .unwrap();
    fs::write(path, bytes).unwrap();
    assert!(load_extraction(&fixture.view(), &extraction_id).is_err());
}
#[test]
fn bounded_unicode_crlf_windows_and_explicit_quote_spans() {
    let content = "# Team\r\n\r\n東 works for Acme.\r\n東 works for Acme.\r\n".as_bytes();
    let fixture = Fixture::new(content);
    let packet = fixture.packet();
    assert_eq!(packet.packet().windows[0].text.as_bytes(), content);
    let start = 10;
    let response = json!({"schema":EXTRACTION_SCHEMA,"packet_id":packet.packet().packet_id,"packet_fingerprint":packet.packet().packet_fingerprint,"mentions":[{"id":"m1","window_id":"w1","label":"東","type":"person","quote":"東","span":{"start":start,"end":start+3}}],"assertions":[],"unresolved":[]});
    assert!(
        validate_response(
            &packet,
            &fixture.view(),
            &serde_json::to_vec(&response).unwrap()
        )
        .is_ok()
    );
    let mut ambiguous = response.clone();
    ambiguous["mentions"][0]
        .as_object_mut()
        .unwrap()
        .remove("span");
    assert!(
        validate_response(
            &packet,
            &fixture.view(),
            &serde_json::to_vec(&ambiguous).unwrap()
        )
        .is_err()
    );
    let mut split = response;
    split["mentions"][0]["span"]["start"] = json!(start + 1);
    assert!(
        validate_response(
            &packet,
            &fixture.view(),
            &serde_json::to_vec(&split).unwrap()
        )
        .is_err()
    );
    let fixture = Fixture::new("東".repeat(80000).as_bytes());
    let plan = build_packet(&fixture.view(), &fixture.request()).unwrap();
    assert_eq!(plan.packet.windows.len(), 16);
    assert!(plan.coverage.omitted_source_bytes > 0);
    assert!(
        plan.packet
            .windows
            .iter()
            .all(|w| w.text.len() <= 12000 && w.text.is_char_boundary(w.text.len()))
    );
}

#[test]
fn output_item_depth_metadata_and_sparse_snapshot_bounds_are_enforced() {
    let fixture = Fixture::new(&text());
    let packet = fixture.packet();
    let response = fixture.response(&packet);
    let mut over = response.clone();
    over["mentions"][0]["label"] = json!("東".repeat(400));
    assert!(
        validate_response(
            &packet,
            &fixture.view(),
            &serde_json::to_vec(&over).unwrap()
        )
        .is_err()
    );
    let mut deep = json!(0);
    for _ in 0..34 {
        deep = json!([deep]);
    }
    over = response.clone();
    over["unexpected"] = deep;
    let error = validate_response(
        &packet,
        &fixture.view(),
        &serde_json::to_vec(&over).unwrap(),
    )
    .unwrap_err();
    assert!(error.message.contains("depth/item"));
    over = response.clone();
    over["unexpected"] = json!(vec![0; 66000]);
    let error = validate_response(
        &packet,
        &fixture.view(),
        &serde_json::to_vec(&over).unwrap(),
    )
    .unwrap_err();
    assert!(error.message.contains("depth/item") || error.message.contains("item limit"));
    over = response.clone();
    over["assertions"][0]["evidence"] =
        json!(vec![response["assertions"][0]["evidence"][0].clone(); 17]);
    assert!(
        validate_response(
            &packet,
            &fixture.view(),
            &serde_json::to_vec(&over).unwrap()
        )
        .is_err()
    );
    let mut assertions = vec![];
    for i in 0..33 {
        let mut assertion = response["assertions"][0].clone();
        assertion["id"] = json!(format!("a{i}"));
        assertion["evidence"] = json!(vec![response["assertions"][0]["evidence"][0].clone(); 16]);
        assertions.push(assertion);
    }
    over = response.clone();
    over["assertions"] = json!(assertions);
    assert!(
        validate_response(
            &packet,
            &fixture.view(),
            &serde_json::to_vec(&over).unwrap()
        )
        .unwrap_err()
        .message
        .contains("total evidence")
    );
    let mut request = fixture.request();
    request.limits.max_output_bytes = 16;
    let small = build_packet(&fixture.view(), &request).unwrap();
    fixture.apply(small.draft.unwrap());
    let small = load_packet(&fixture.view(), &small.packet.packet_id).unwrap();
    assert!(
        validate_response(
            &small,
            &fixture.view(),
            &serde_json::to_vec(&response).unwrap()
        )
        .is_err()
    );
    request.limits.max_output_bytes = 262145;
    assert!(build_packet(&fixture.view(), &request).is_err());
    let revision_path = fixture.root.path().join(format!(
        "sources/{}/revisions/{}/revision.md",
        fixture.source, fixture.revision
    ));
    let note = lwiki::records::parse_note(&fs::read(&revision_path).unwrap());
    let record = note.canonical.unwrap();
    for key in ["wiki_original_path", "wiki_content_path"] {
        let path = revision_path
            .parent()
            .unwrap()
            .join(record.string(key).unwrap());
        let original = fs::read(&path).unwrap();
        let file = fs::File::create(&path).unwrap();
        file.set_len(64 * 1024 * 1024 + 1).unwrap();
        drop(file);
        assert_eq!(
            build_packet(&fixture.view(), &fixture.request())
                .unwrap_err()
                .code,
            ErrorCode::BudgetExceeded
        );
        let mut excessive = fixture.request();
        excessive.windows = vec![ByteSpan::new(0, 1).unwrap(); 17];
        assert!(
            build_packet(&fixture.view(), &excessive)
                .unwrap_err()
                .message
                .contains("too many requested")
        );
        fs::write(path, original).unwrap();
    }
}

#[test]
fn candidate_context_is_canonical_bounded_and_unknown_nested_fields_refuse() {
    let fixture = Fixture::new(&text());
    let mut request = fixture.request();
    request.candidate_context = vec![CandidateIdentity {
        reference: RecordRef {
            vault_id: id(VAULT),
            record_id: id("entity_00000000-0000-7000-8000-000000000001"),
            expected_kind: RecordKind::Entity,
        },
        title: "Alex Kim".into(),
        entity_type: "person".into(),
    }];
    let plan = build_packet(&fixture.view(), &request).unwrap();
    let packet_id = plan.packet.packet_id.clone();
    let target = plan.locator.path.clone();
    fixture.apply(plan.draft.unwrap());
    let original = fs::read(fixture.root.path().join(target.as_str())).unwrap();
    let note = lwiki::records::parse_note(&original);
    let mut value = serde_json::to_value(&plan.packet).unwrap();
    value["candidate_context"][0]["reference"]["extra"] = json!(true);
    let body = format!(
        "\n```{PACKET_FENCE}\n{}\n```\n",
        serde_json::to_string(&value).unwrap()
    );
    let mut bytes = note.raw[..note.raw.len() - note.body().len()].to_vec();
    bytes.extend_from_slice(body.as_bytes());
    fs::write(fixture.root.path().join(target.as_str()), bytes).unwrap();
    assert!(
        load_packet(&fixture.view(), &packet_id)
            .unwrap_err()
            .message
            .contains("RecordRef")
    );
    fs::write(fixture.root.path().join(target.as_str()), &original).unwrap();
    let entity_path = fixture.root.path().join("knowledge/entities/alex_north.md");
    let entity = lwiki::records::parse_note(&fs::read(&entity_path).unwrap());
    let renamed = lwiki::records::edit_note(
        &entity,
        &BTreeMap::from([("title".into(), json!("Renamed identity"))]),
        None,
        &entity.source_hash,
    )
    .unwrap();
    fs::write(entity_path, renamed).unwrap();
    assert!(load_packet(&fixture.view(), &packet_id).is_ok());
    assert!(build_packet(&fixture.view(), &request).is_err());
}

#[test]
fn canonical_matching_response_cannot_reuse_another_retained_allocation() {
    let fixture = Fixture::new(&text());
    let packet = fixture.packet();
    let response = fixture.response(&packet);
    let original = fixture.stage(&packet, &response, OriginPolicy::ReuseOrConflict);
    let inspection = fixture
        .engine
        .inspect(&original.prepared.as_ref().unwrap().change_id)
        .unwrap();
    let op = inspection
        .manifest
        .operations
        .iter()
        .find(|op| op.target == original.extraction.path)
        .unwrap();
    let payload = op.after_payload.as_ref().unwrap();
    let bytes = fs::read(fixture.root.path().join(payload.path.as_str())).unwrap();
    let note = lwiki::records::parse_note(&bytes);
    let body = std::str::from_utf8(note.body()).unwrap();
    let marker = format!("```{ARTIFACT_FENCE}\n");
    let json = body
        .split_once(&marker)
        .unwrap()
        .1
        .split_once("\n```")
        .unwrap()
        .0;
    let mut artifact: ExtractionArtifactV1 = serde_json::from_str(json).unwrap();
    artifact.extraction_id = id("extraction_alternate");
    artifact
        .allocations
        .assertions
        .insert(local("a1"), id("assertion_alternate"));
    artifact
        .allocations
        .evidence
        .insert(local("a1"), vec![id("evidence_alternate")]);
    let mut fields = note.canonical.unwrap().into_fields();
    fields.insert("wiki_id".into(), json!(artifact.extraction_id));
    let bytes = record(
        serde_json::to_value(fields).unwrap(),
        &artifact_body(&artifact).unwrap(),
    );
    let alternate = fixture
        .root
        .path()
        .join("knowledge/extractions/extraction_alternate.md");
    fs::write(alternate, bytes).unwrap();
    assert!(load_extraction(&fixture.view(), &artifact.extraction_id).is_ok());
    let validated = validate_response(
        &packet,
        &fixture.view(),
        &serde_json::to_vec(&response).unwrap(),
    )
    .unwrap();
    let error = stage_import(
        &fixture.engine,
        &fixture.writer(),
        &validated,
        OriginPolicy::ReuseOrConflict,
    )
    .unwrap_err();
    assert!(error.message.contains("immutable identities/allocations"));
    assert_eq!(
        fixture
            .engine
            .inspect(&inspection.prepared.change_id)
            .unwrap()
            .status,
        ChangeStatus::Prepared
    );
    assert!(
        !fixture
            .root
            .path()
            .join(original.extraction.path.as_str())
            .exists()
    );
}

#[test]
fn restoration_keeps_original_allocations_after_review_and_successor_revalidation() {
    let fixture = Fixture::new(&text());
    let packet = fixture.packet();
    let response = fixture.response(&packet);
    let outcome = fixture.stage(&packet, &response, OriginPolicy::ReuseOrConflict);
    fixture.apply_import(&outcome);
    let mut artifact = explicit_binding(&fixture, &outcome, true);
    let assertion_id = artifact.allocations.assertions[&local("a1")].clone();
    let evidence_id = artifact.allocations.evidence[&local("a1")][0].clone();
    let assertion_path =
        VaultRelativePath::new(format!("knowledge/assertions/{assertion_id}.md")).unwrap();
    let evidence_path =
        VaultRelativePath::new(format!("knowledge/evidence/{evidence_id}.md")).unwrap();
    let assertion = record(
        json!({"wiki_schema":"1","wiki_id":assertion_id,"wiki_kind":"assertion","title":"Explicitly bound fixture assertion","wiki_status":"proposed","wiki_subject_id":"entity_00000000-0000-7000-8000-000000000001","wiki_predicate":"works_for","wiki_object_id":"entity_00000000-0000-7000-8000-000000000003","wiki_negated":false,"wiki_modality":"asserted"}),
        b"\nA retained source-local proposal after explicit fixture bindings.\n",
    );
    let proof = &artifact.evidence_spans[&local("a1")][0];
    let evidence = record(
        json!({"wiki_schema":"1","wiki_id":evidence_id,"wiki_kind":"evidence","title":"Exact reserved evidence","wiki_status":"active","wiki_assertion_id":assertion_id,"wiki_source_id":artifact.source_id,"wiki_source_revision":artifact.source_revision,"wiki_stance":"supports","wiki_locator_kind":"utf8-bytes","wiki_span_start":proof.span.start(),"wiki_span_end":proof.span.end(),"wiki_quote_hash":proof.quote_hash,"wiki_extraction_id":artifact.extraction_id}),
        &lwiki::sources::evidence::exact_quote_body(
            b"Ada works for Acme.",
            "\n",
            "Fixture support, structural trace only.",
        )
        .unwrap(),
    );
    artifact.materialized_assertions.push(local("a1"));
    let update = artifact_edit(&fixture, &outcome, &artifact);
    fixture.apply(ChangeDraft {
        title: "Materialize explicitly bound fixture proposals".into(),
        origin: None,
        inverse_of: None,
        allocated_ids: BTreeMap::new(),
        read_preconditions: vec![],
        operations: vec![
            ExpectedWrite {
                target: assertion_path.clone(),
                expected: ExpectedState::Absent,
                proposed: Some(assertion),
                apply_after: vec![],
            },
            ExpectedWrite {
                target: evidence_path.clone(),
                expected: ExpectedState::Absent,
                proposed: Some(evidence),
                apply_after: vec![],
            },
            update,
        ],
    });
    let extraction_id = artifact.extraction_id.clone();
    assert_eq!(
        load_extraction(&fixture.view(), &extraction_id)
            .unwrap()
            .artifact()
            .materialized_assertions,
        [local("a1")]
    );
    for status in ["accepted", "rejected"] {
        let raw = fs::read(fixture.root.path().join(assertion_path.as_str())).unwrap();
        let note = lwiki::records::parse_note(&raw);
        let edited = lwiki::records::edit_note(
            &note,
            &BTreeMap::from([("wiki_status".into(), json!(status))]),
            None,
            &note.source_hash,
        )
        .unwrap();
        fixture.apply(ChangeDraft {
            title: format!("Explicit fixture status {status}"),
            origin: None,
            inverse_of: None,
            allocated_ids: BTreeMap::new(),
            read_preconditions: vec![],
            operations: vec![ExpectedWrite {
                target: assertion_path.clone(),
                expected: ExpectedState::Hash(note.source_hash),
                proposed: Some(edited),
                apply_after: vec![],
            }],
        });
        assert!(load_extraction(&fixture.view(), &extraction_id).is_ok());
    }
    let next = [text(), b"\nSuccessor context\n".to_vec()].concat();
    let plan = SourceStore::new(fixture.engine.fs().clone())
        .plan_refresh(
            &fixture.source,
            CaptureRequest {
                title: "Packet test source".into(),
                origin_kind: SourceOrigin::LocalFile,
                origin: "synthetic.md".into(),
                original: next,
                extraction: ExtractionInput::Utf8Preserve,
                media_type: Some("text/markdown".into()),
            },
        )
        .unwrap();
    let revision = plan.revision_id;
    fixture.apply(plan.draft.unwrap());
    let note = lwiki::records::parse_note(
        &fs::read(fixture.root.path().join(evidence_path.as_str())).unwrap(),
    );
    let successor = SourceStore::new(fixture.engine.fs().clone())
        .plan_revalidate(&evidence_id, &revision, &note.source_hash)
        .unwrap();
    let successor_id = successor.evidence_id;
    fixture.apply(successor.draft);
    let loaded = load_extraction(&fixture.view(), &extraction_id).unwrap();
    assert_eq!(loaded.artifact().allocations, outcome.allocations);
    assert_ne!(successor_id, evidence_id);
    let original = lwiki::records::parse_note(
        &fs::read(fixture.root.path().join(evidence_path.as_str())).unwrap(),
    );
    assert_eq!(
        original.canonical.as_ref().unwrap().string("wiki_status"),
        Some("active")
    );
    // P04 revalidation retains the active historical predecessor. A later explicit
    // review may retract it; neither state changes the original reserved maps.
    let retracted = lwiki::records::edit_note(
        &original,
        &BTreeMap::from([("wiki_status".into(), json!("retracted"))]),
        None,
        &original.source_hash,
    )
    .unwrap();
    fixture.apply(ChangeDraft {
        title: "Explicit fixture predecessor retraction".into(),
        origin: None,
        inverse_of: None,
        allocated_ids: BTreeMap::new(),
        read_preconditions: vec![],
        operations: vec![ExpectedWrite {
            target: evidence_path,
            expected: ExpectedState::Hash(original.source_hash),
            proposed: Some(retracted),
            apply_after: vec![],
        }],
    });
    assert_eq!(
        load_extraction(&fixture.view(), &extraction_id)
            .unwrap()
            .artifact()
            .allocations,
        outcome.allocations
    );
    let packet = load_packet(&fixture.view(), &packet.packet().packet_id).unwrap();
    let reused = fixture.stage(&packet, &response, OriginPolicy::ReuseOrConflict);
    assert_eq!(reused.allocations, outcome.allocations);
    assert_eq!(reused.coverage.materialized_assertions, 1);
    let asserted = lwiki::records::parse_note(
        &fs::read(fixture.root.path().join(assertion_path.as_str())).unwrap(),
    );
    assert_eq!(
        asserted.canonical.unwrap().string("wiki_status"),
        Some("rejected")
    );
}

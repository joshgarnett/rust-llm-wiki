use super::*;
use crate::{
    domain::{Blake3Hash, CanonicalRecord, VaultRelativePath},
    records::RegistryEntry,
};
use serde_json::json;
fn id(value: &str) -> RecordId {
    RecordId::new(value).unwrap()
}
fn row(kind: &str, name: &str, fields: Value) -> RecordRow {
    let mut values = BTreeMap::from([
        ("wiki_schema".into(), json!("1")),
        ("wiki_kind".into(), json!(kind)),
        ("wiki_id".into(), json!(name)),
        ("title".into(), json!(name)),
    ]);
    values.extend(
        fields
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    let record = CanonicalRecord::new(values).unwrap();
    RecordRow {
        authored_status: record.string("wiki_status").map(str::to_owned),
        record,
        path: VaultRelativePath::new(format!("{name}.md")).unwrap(),
        hash: Blake3Hash::digest(name),
        eligibility: Eligibility::Current,
        reasons: vec![],
        identity_eligibility: None,
        description_eligibility: None,
        disputed: false,
        dependencies: vec![],
    }
}
fn source() -> RecordRow {
    row(
        "source",
        "source_fixture",
        json!({"wiki_status":"active","wiki_origin_kind":"local-file","wiki_origin":"fixture.txt","wiki_current_revision":"revision_future","wiki_revisions":["revision_future"]}),
    )
}
fn revision() -> RecordRow {
    row(
        "revision",
        "revision_future",
        json!({"wiki_source_id":"source_fixture","wiki_original_path":"original.bin","wiki_original_hash":Blake3Hash::digest(b"quote"),"wiki_content_path":"content.md","wiki_content_hash":Blake3Hash::digest(b"quote"),"wiki_extractor":"fixture","wiki_extractor_fingerprint":Blake3Hash::digest(b"fixture"),"wiki_extraction_status":"complete","wiki_captured_at":"2026-10-03T00:00:00Z"}),
    )
}
fn registry(rows: &BTreeMap<RecordId, RecordRow>) -> IndexedRegistry {
    IndexedRegistry::new(
        rows.values()
            .map(|r| RegistryEntry {
                id: r.record.id().clone(),
                kind: r.record.kind(),
                path: r.path.clone(),
                aliases: vec![],
            })
            .collect(),
    )
}
#[test]
fn missing_revision_reference_repairs_with_or_without_companion() {
    for companion in [false, true] {
        let fingerprint = Blake3Hash::digest(b"packet");
        let mut fields = json!({"wiki_source_id":"source_fixture","wiki_source_revision":"revision_future","wiki_packet_fingerprint":fingerprint,"wiki_output_schema":"fixture","wiki_created_at":"2026-10-03T00:00:00Z"});
        if companion {
            fields["wiki_revision"] = json!("[[revision_future]]");
        }
        let packet = row(
            "extraction_packet",
            RecordId::packet(&fingerprint).as_str(),
            fields,
        );
        let before = BTreeMap::from([(id("source_fixture"), source())]);
        let missing = BTreeSet::from([id("revision_future")]);
        let mut invalid = packet.clone();
        let mut diagnostics = vec![];
        let mut recorder = StructuralRecorder::new(true);
        let edges = evaluate_references(
            &mut invalid,
            &registry(&before),
            &ReferenceBoundary::selected(&before, &missing).unwrap(),
            &mut diagnostics,
            &mut recorder,
        )
        .unwrap();
        assert_eq!(invalid.eligibility, Eligibility::Invalid);
        assert_eq!(
            invalid.reasons,
            vec!["invalid_reference:wiki_source_revision"]
        );
        assert!(edges.targets.contains(&id("revision_future")));
        let facts = recorder.finish();
        assert_eq!(facts[packet.record.id()].diagnostics(&packet), diagnostics);
        let mut after = before;
        after.insert(id("revision_future"), revision());
        let mut repaired = packet.clone();
        let mut diagnostics = vec![];
        let mut recorder = StructuralRecorder::new(true);
        evaluate_references(
            &mut repaired,
            &registry(&after),
            &ReferenceBoundary::selected(&after, &BTreeSet::new()).unwrap(),
            &mut diagnostics,
            &mut recorder,
        )
        .unwrap();
        assert_eq!(repaired.eligibility, Eligibility::Current);
        assert!(repaired.reasons.is_empty());
        assert!(diagnostics.is_empty());
        assert!(recorder.finish().is_empty());
    }
}
#[test]
fn selected_reference_boundary_distinguishes_missing_from_unqueried_before_mutation() {
    let mut current = source();
    let before = current.clone();
    let rows = BTreeMap::new();
    let missing = BTreeSet::new();
    let boundary = ReferenceBoundary::selected(&rows, &missing).unwrap();
    let mut diagnostics = vec![];
    let mut recorder = StructuralRecorder::new(true);
    let error = evaluate_references(
        &mut current,
        &registry(&rows),
        &boundary,
        &mut diagnostics,
        &mut recorder,
    )
    .err()
    .unwrap();
    assert_eq!(error.code, ErrorCode::IndexCorrupt);
    assert_eq!(current, before);
    assert!(diagnostics.is_empty());
}
#[test]
fn propagation_repairs_cycles_from_local_state_and_preserves_entity_identity() {
    let mut rows = BTreeMap::from([
        (id("source_fixture"), source()),
        (id("revision_future"), revision()),
    ]);
    let edges = BTreeMap::from([
        (
            id("source_fixture"),
            BTreeSet::from([id("revision_future")]),
        ),
        (
            id("revision_future"),
            BTreeSet::from([id("source_fixture")]),
        ),
    ]);
    let mut diagnostics = vec![];
    let mut recorder = StructuralRecorder::new(true);
    recorder.phase(StructuralStage::Propagation);
    rows.get_mut(&id("revision_future")).unwrap().eligibility = Eligibility::Invalid;
    propagate_invalid(
        &mut rows,
        &edges,
        &mut diagnostics,
        &BTreeMap::new(),
        &mut recorder,
    )
    .unwrap();
    assert_eq!(
        rows[&id("source_fixture")].eligibility,
        Eligibility::Invalid
    );
    let mut repaired = BTreeMap::from([
        (id("source_fixture"), source()),
        (id("revision_future"), revision()),
    ]);
    propagate_invalid(
        &mut repaired,
        &edges,
        &mut vec![],
        &BTreeMap::new(),
        &mut StructuralRecorder::new(false),
    )
    .unwrap();
    assert!(
        repaired
            .values()
            .all(|r| r.eligibility == Eligibility::Current)
    );
    let entity = row(
        "entity",
        "entity_fixture",
        json!({"wiki_status":"active","wiki_entity_type":"component","wiki_depends_on_ids":["assertion_bad"]}),
    );
    let mut selected = BTreeMap::from([(id("entity_fixture"), entity)]);
    let edges = BTreeMap::from([(id("entity_fixture"), BTreeSet::from([id("assertion_bad")]))]);
    propagate_invalid(
        &mut selected,
        &edges,
        &mut vec![],
        &BTreeMap::from([(id("assertion_bad"), true)]),
        &mut StructuralRecorder::new(false),
    )
    .unwrap();
    assert_eq!(
        selected[&id("entity_fixture")].eligibility,
        Eligibility::Current
    );
    assert!(
        propagate_invalid(
            &mut selected,
            &edges,
            &mut vec![],
            &BTreeMap::new(),
            &mut StructuralRecorder::new(false)
        )
        .is_err()
    );
}
#[test]
fn retained_effects_preserve_null_diagnostics_repeats_and_producer() {
    let mut record = source();
    let mut diagnostics = vec![];
    let mut recorder = StructuralRecorder::new(true);
    recorder.phase(StructuralStage::Decision);
    recorder.producer(Some(id("decision_fixture")));
    for _ in 0..2 {
        invalidate(
            &mut record,
            "same_reason",
            &mut diagnostics,
            Value::Null,
            &mut recorder,
        );
    }
    state(
        &mut record,
        Eligibility::Historical,
        "historic",
        &mut recorder,
    );
    let fact = recorder.finish().remove(record.record.id()).unwrap();
    let round: StructuralFact =
        serde_json::from_str(&serde_json::to_string(&fact).unwrap()).unwrap();
    assert_eq!(round, fact);
    assert_eq!(round.diagnostics(&record), diagnostics);
    assert_eq!(round.baseline().reasons, vec!["historic", "same_reason"]);
    assert_eq!(round.baseline().eligibility, Eligibility::Historical);
    assert_eq!(round.effects[0].details, Some(Value::Null));
    assert_eq!(round.effects[2].details, None);
}

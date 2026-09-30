use lwiki::{
    catalog::{
        CatalogGraphValidator,
        scan::{project, scan},
    },
    changes::{GraphValidator, ProposedTarget, ScanDocument, ValidationInput},
    domain::*,
    records::parse_note,
    sources::{evidence::exact_quote_body, *},
    vault::{VaultFs, VaultRoot},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path};

fn id(s: &str) -> RecordId {
    RecordId::new(s).unwrap()
}
fn path(s: &str) -> VaultRelativePath {
    VaultRelativePath::new(s).unwrap()
}
fn bytes(kind: &str, name: &str, extra: Value, body: &[u8]) -> Vec<u8> {
    let mut fields = BTreeMap::from([
        ("wiki_schema".into(), json!("1")),
        ("wiki_id".into(), json!(name)),
        ("wiki_kind".into(), json!(kind)),
        ("title".into(), json!(name)),
    ]);
    fields.extend(
        extra
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone())),
    );
    CanonicalRecord::new(fields.clone()).unwrap();
    let mut result = b"---\n".to_vec();
    for (key, value) in fields {
        result.extend_from_slice(format!("{key}: {value}\n").as_bytes());
    }
    result.extend_from_slice(b"---\n");
    result.extend_from_slice(body);
    result
}
fn write(root: &Path, name: &str, data: &[u8]) {
    let target = root.join(name);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(target, data).unwrap();
}
fn fixture() -> (tempfile::TempDir, VaultFs) {
    let temp = tempfile::tempdir().unwrap();
    write(
        temp.path(),
        "WIKI.md",
        &bytes("vault", "vault_test", json!({}), b"Wiki\n"),
    );
    for name in ["alpha", "beta"] {
        write(
            temp.path(),
            &format!("entities/{name}.md"),
            &bytes(
                "entity",
                name,
                json!({"wiki_status":"active","wiki_entity_type":"component"}),
                b"Unsupported description\n",
            ),
        );
    }
    write(
        temp.path(),
        "assertions/use.md",
        &bytes(
            "assertion",
            "use",
            json!({"wiki_status":"accepted","wiki_subject_id":"alpha","wiki_object_id":"beta","wiki_predicate":"uses"}),
            b"Alpha uses Beta.\n",
        ),
    );
    add_source(
        temp.path(),
        "source_a",
        "revision_a",
        b"Alpha uses Beta.\n",
        true,
    );
    add_source(
        temp.path(),
        "source_b",
        "revision_b",
        b"Alpha uses Beta.\n",
        true,
    );
    add_evidence(
        temp.path(),
        "evidence_a",
        "source_a",
        "revision_a",
        "supports",
    );
    add_evidence(
        temp.path(),
        "evidence_b",
        "source_b",
        "revision_b",
        "supports",
    );
    add_evidence(
        temp.path(),
        "contradiction",
        "source_a",
        "revision_a",
        "contradicts",
    );
    write(
        temp.path(),
        "page.md",
        &bytes(
            "page",
            "page",
            json!({"wiki_status":"reviewed","wiki_depends_on_ids":["use"]}),
            b"# Supported page\n",
        ),
    );
    let root = VaultRoot::explicit(temp.path()).unwrap();
    (temp, VaultFs::new(root))
}
fn add_source(root: &Path, source: &str, revision: &str, content: &[u8], complete: bool) {
    let hash = Blake3Hash::digest(content);
    write(
        root,
        &format!("sources/{source}/source.md"),
        &bytes(
            "source",
            source,
            json!({"wiki_status":"active","wiki_origin_kind":"local-file","wiki_origin":"fixture","wiki_current_revision":revision,"wiki_revisions":[revision]}),
            b"Synthetic source\n",
        ),
    );
    let mut fields = json!({"wiki_source_id":source,"wiki_captured_at":"2026-09-28T00:00:00Z","wiki_original_path":"original.bin","wiki_original_hash":hash,"wiki_extractor":"fixture","wiki_extractor_fingerprint":Blake3Hash::digest(b"fixture-v1"),"wiki_extraction_status":if complete{"complete"}else{"unsupported"}});
    if complete {
        fields["wiki_content_path"] = json!("content.md");
        fields["wiki_content_hash"] = json!(hash);
    }
    let directory = format!("sources/{source}/revisions/{revision}");
    write(
        root,
        &format!("{directory}/revision.md"),
        &bytes("revision", revision, fields, b"Revision\n"),
    );
    write(root, &format!("{directory}/original.bin"), content);
    if complete {
        write(root, &format!("{directory}/content.md"), content);
    }
}
fn add_evidence(root: &Path, evidence: &str, source: &str, revision: &str, stance: &str) {
    let quote = b"Alpha uses Beta.";
    write(
        root,
        &format!("evidence/{evidence}.md"),
        &bytes(
            "evidence",
            evidence,
            json!({"wiki_status":"active","wiki_assertion_id":"use","wiki_source_id":source,"wiki_source_revision":revision,"wiki_stance":stance,"wiki_locator_kind":"utf8-bytes","wiki_span_start":0,"wiki_span_end":quote.len(),"wiki_quote_hash":Blake3Hash::digest(quote)}),
            &exact_quote_body(quote, "\n", "Explanation").unwrap(),
        ),
    );
}
fn input(fs: &VaultFs) -> ValidationInput {
    ValidationInput {
        vault_id: id("vault_test"),
        documents: fs
            .root()
            .scan_markdown()
            .unwrap()
            .into_iter()
            .map(|path| {
                let before = fs.read_before(&path).unwrap().unwrap();
                ScanDocument {
                    path,
                    bytes: before.bytes,
                    hash: before.hash,
                }
            })
            .collect(),
        overlay: vec![],
    }
}
fn edit(root: &Path, p: &str, key: &str, value: Value) {
    let old = fs::read(root.join(p)).unwrap();
    let note = parse_note(&old);
    let mut fields = note.canonical.as_ref().unwrap().fields().clone();
    fields.insert(key.into(), value);
    let kind = fields["wiki_kind"].as_str().unwrap().to_owned();
    let name = fields["wiki_id"].as_str().unwrap().to_owned();
    write(root, p, &bytes(&kind, &name, json!(fields), note.body()));
}

#[test]
fn copied_ids_and_companion_conflicts_exclude_all() {
    let (temp, fs) = fixture();
    let original = fs::read(temp.path().join("entities/alpha.md")).unwrap();
    let invalid = String::from_utf8(original.clone())
        .unwrap()
        .replace("wiki_entity_type: \"component\"", "wiki_entity_type: 42");
    write(temp.path(), "copy.md", invalid.as_bytes());
    let p = scan(&fs, &id("vault_test")).unwrap();
    assert!(!p.records.contains_key(&id("alpha")));
    let copies: Vec<_> = p
        .diagnostics
        .iter()
        .filter(|d| d.code == ErrorCode::ReferenceAmbiguous)
        .collect();
    assert_eq!(copies.len(), 2);
    assert!(
        p.documents
            .iter()
            .filter(|d| d.path.as_str() == "copy.md" || d.path.as_str() == "entities/alpha.md")
            .all(|d| d.record_id.is_none() && !d.raw_text.is_empty())
    );
    fs::remove_file(temp.path().join("copy.md")).unwrap();
    write(
        temp.path(),
        "copy.md",
        String::from_utf8(original)
            .unwrap()
            .replace("wiki_schema: \"1\"", "wiki_schema: \"2\"")
            .as_bytes(),
    );
    assert!(
        !scan(&fs, &id("vault_test"))
            .unwrap()
            .records
            .contains_key(&id("alpha"))
    );
    fs::remove_file(temp.path().join("copy.md")).unwrap();
    write(
        temp.path(),
        "copy.md",
        b"---\nwiki_id: alpha\nwiki_id: beta\nwiki_kind: entity\n---\nMalformed duplicate keys\n",
    );
    let malformed = scan(&fs, &id("vault_test")).unwrap();
    assert!(!malformed.records.contains_key(&id("alpha")));
    assert!(!malformed.records.contains_key(&id("beta")));
    assert_eq!(
        malformed
            .diagnostics
            .iter()
            .filter(|d| d.code == ErrorCode::ReferenceAmbiguous)
            .count(),
        4
    );
    fs::remove_file(temp.path().join("copy.md")).unwrap();
    for scalar in [
        "alpha # explanation",
        "\"alpha\" # explanation",
        "'alpha' # explanation",
        "\"\\u0061lpha\" # explanation",
    ] {
        write(temp.path(),"copy.md",format!("---\nwiki_id: {scalar}\nother: [unfinished\n---\nMalformed flow retains claimed identity\n").as_bytes());
        let malformed = scan(&fs, &id("vault_test")).unwrap();
        assert!(!malformed.records.contains_key(&id("alpha")), "{scalar}");
        assert_eq!(
            malformed
                .diagnostics
                .iter()
                .filter(|d| d.code == ErrorCode::ReferenceAmbiguous)
                .count(),
            2,
            "{scalar}"
        );
    }
    fs::remove_file(temp.path().join("copy.md")).unwrap();
    for field in [
        "'wiki_id': alpha",
        "\"wiki_id\": alpha",
        "wiki_id : alpha",
        "wiki_id: >-\n  alpha",
        "wiki_id: |-\n  alpha",
    ] {
        write(
            temp.path(),
            "copy.md",
            format!("---\n{field}\nother: [unfinished\n---\nMalformed body\n").as_bytes(),
        );
        let malformed = scan(&fs, &id("vault_test")).unwrap();
        assert!(!malformed.records.contains_key(&id("alpha")), "{field}");
        assert_eq!(
            malformed
                .diagnostics
                .iter()
                .filter(|d| d.code == ErrorCode::ReferenceAmbiguous)
                .count(),
            2,
            "{field}"
        );
    }
    fs::remove_file(temp.path().join("copy.md")).unwrap();
    edit(
        temp.path(),
        "assertions/use.md",
        "wiki_subject",
        json!("[[entities/beta.md]]"),
    );
    let p = scan(&fs, &id("vault_test")).unwrap();
    assert_eq!(p.records[&id("use")].eligibility, Eligibility::Invalid);
}

#[test]
fn evidence_navigation_reports_missing_wrong_kind_and_wrong_membership() {
    let (temp, fs) = fixture();
    write(
        temp.path(),
        "assertions/other.md",
        &bytes(
            "assertion",
            "other",
            json!({"wiki_status":"proposed","wiki_subject_id":"alpha","wiki_object_id":"beta","wiki_predicate":"uses"}),
            b"Other assertion\n",
        ),
    );
    edit(
        temp.path(),
        "evidence/evidence_b.md",
        "wiki_assertion_id",
        json!("other"),
    );
    edit(
        temp.path(),
        "assertions/use.md",
        "wiki_evidence",
        json!([
            "[[evidence/missing.md]]",
            "[[entities/alpha.md]]",
            "[[evidence/evidence_b.md]]",
            "[[evidence/evidence_a.md]]"
        ]),
    );
    let projected = scan(&fs, &id("vault_test")).unwrap();
    let reasons: Vec<_> = projected
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.path.as_str() == "assertions/use.md")
        .filter_map(|diagnostic| diagnostic.details["reason"].as_str())
        .collect();
    assert!(reasons.contains(&"evidence_link_missing"));
    assert!(reasons.contains(&"evidence_link_wrong_kind"));
    assert!(reasons.contains(&"evidence_link_wrong_assertion"));
    assert_eq!(
        reasons
            .iter()
            .filter(|reason| reason.starts_with("evidence_link_"))
            .count(),
        3
    );
    assert_eq!(
        projected.records[&id("use")].eligibility,
        Eligibility::Current
    );
}

#[test]
fn malformed_decision_keeps_audit_kind_without_losing_raw_text() {
    let (temp, fs) = fixture();
    write(
        temp.path(),
        "knowledge/decisions/broken.md",
        b"---\nwiki_kind: decision\nwiki_id: decision_broken\ntitle: [broken\n---\nRetained editorial rationale\n",
    );
    let projection = scan(&fs, &id("vault_test")).unwrap();
    let document = projection
        .documents
        .iter()
        .find(|document| document.path.as_str() == "knowledge/decisions/broken.md")
        .unwrap();
    assert_eq!(document.kind, Some(RecordKind::Decision));
    assert!(document.raw_text.contains("Retained editorial rationale"));
    assert_eq!(document.eligibility, Eligibility::Invalid);
}
#[test]
fn graph_endpoints_use_resolved_names_and_keep_direction() {
    let (temp, fs) = fixture();
    edit(
        temp.path(),
        "entities/alpha.md",
        "title",
        json!("Subject name"),
    );
    edit(
        temp.path(),
        "entities/alpha.md",
        "aliases",
        json!(["Subject alias"]),
    );
    edit(
        temp.path(),
        "entities/beta.md",
        "title",
        json!("Object name"),
    );
    let projection = scan(&fs, &id("vault_test")).unwrap();
    let row = projection
        .graph
        .iter()
        .find(|row| row.target_id == id("use"))
        .unwrap();
    assert_eq!(row.endpoints, "Subject name Subject alias Object name");
    assert_eq!(
        projection.records[&id("use")]
            .record
            .string("wiki_subject_id"),
        Some("alpha")
    );
    assert_eq!(
        projection.records[&id("use")]
            .record
            .string("wiki_object_id"),
        Some("beta")
    );
}

#[test]
fn same_timestamp_edit_and_decision_change_detected() {
    let (temp, fs) = fixture();
    let first = scan(&fs, &id("vault_test")).unwrap();
    let target = temp.path().join("page.md");
    let file = fs::File::open(&target).unwrap();
    let times = file.metadata().unwrap().modified().unwrap();
    let old = fs::read(&target).unwrap();
    let edited = String::from_utf8(old.clone())
        .unwrap()
        .replace("Supported page", "Supported note");
    assert_eq!(edited.len(), old.len());
    fs::write(&target, edited).unwrap();
    fs::File::options()
        .write(true)
        .open(&target)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(times))
        .unwrap();
    assert_eq!(fs::metadata(&target).unwrap().modified().unwrap(), times);
    let second = scan(&fs, &id("vault_test")).unwrap();
    assert_ne!(first.control_manifest, second.control_manifest);
    write(
        temp.path(),
        "decision.md",
        &bytes(
            "decision",
            "decision",
            json!({"wiki_status":"active","wiki_action":"accept","wiki_input_ids":["use"],"wiki_output_ids":["use"],"wiki_created_at":"2026-09-28T00:00:00Z"}),
            b"Explicit acceptance\n",
        ),
    );
    let third = scan(&fs, &id("vault_test")).unwrap();
    assert_ne!(second.control_manifest, third.control_manifest);
    assert_eq!(third.records[&id("use")].eligibility, Eligibility::Current);
    let again = scan(&fs, &id("vault_test")).unwrap();
    assert_eq!(third, again);
}

#[test]
fn dependency_cycle_and_identity_description_split() {
    let (temp, fs) = fixture();
    let p = scan(&fs, &id("vault_test")).unwrap();
    assert_eq!(
        p.records[&id("alpha")].identity_eligibility,
        Some(Eligibility::Current)
    );
    assert_eq!(
        p.records[&id("alpha")].description_eligibility,
        Some(Eligibility::Unsupported)
    );
    assert!(
        p.graph
            .iter()
            .find(|g| g.target_id == id("alpha"))
            .unwrap()
            .description
            .is_empty()
    );
    edit(
        temp.path(),
        "entities/alpha.md",
        "wiki_depends_on_ids",
        json!(["use"]),
    );
    edit(
        temp.path(),
        "assertions/use.md",
        "wiki_depends_on_ids",
        json!(["use"]),
    );
    let p = scan(&fs, &id("vault_test")).unwrap();
    assert_eq!(p.records[&id("use")].eligibility, Eligibility::Invalid);
    assert_eq!(p.records[&id("page")].eligibility, Eligibility::Invalid);
    assert_eq!(
        p.records[&id("alpha")].identity_eligibility,
        Some(Eligibility::Current)
    );
    assert_eq!(
        p.records[&id("alpha")].description_eligibility,
        Some(Eligibility::Invalid)
    );
}

#[test]
fn one_then_all_support_withdrawal_recomputes_full_closure() {
    let (temp, fs) = fixture();
    edit(
        temp.path(),
        "entities/alpha.md",
        "wiki_depends_on_ids",
        json!(["use"]),
    );
    assert!(scan(&fs, &id("vault_test")).unwrap().records[&id("use")].disputed);
    let mut proposal = input(&fs);
    let source = fs::read(temp.path().join("sources/source_a/source.md")).unwrap();
    proposal.overlay.push(ProposedTarget {
        path: path("sources/source_a/source.md"),
        bytes: Some(
            String::from_utf8(source)
                .unwrap()
                .replace("wiki_status: \"active\"", "wiki_status: \"withdrawn\"")
                .into_bytes(),
        ),
    });
    CatalogGraphValidator.validate(&fs, &proposal).unwrap();
    let p = project(&fs, &proposal).unwrap();
    assert_eq!(p.records[&id("use")].eligibility, Eligibility::Current);
    assert!(!p.records[&id("use")].disputed);
    edit(
        temp.path(),
        "sources/source_a/source.md",
        "wiki_status",
        json!("withdrawn"),
    );
    edit(
        temp.path(),
        "sources/source_b/source.md",
        "wiki_status",
        json!("withdrawn"),
    );
    let p = scan(&fs, &id("vault_test")).unwrap();
    assert_eq!(p.records[&id("use")].eligibility, Eligibility::Unsupported);
    assert_eq!(p.records[&id("page")].eligibility, Eligibility::Stale);
    assert_eq!(
        p.records[&id("alpha")].identity_eligibility,
        Some(Eligibility::Current)
    );
    assert_eq!(
        p.records[&id("alpha")].description_eligibility,
        Some(Eligibility::Stale)
    );
}

#[test]
fn source_advance_preserves_historical_evidence_and_stales_descendants() {
    let (temp, fs) = fixture();
    edit(
        temp.path(),
        "sources/source_b/source.md",
        "wiki_status",
        json!("withdrawn"),
    );
    add_source(
        temp.path(),
        "source_a",
        "revision_next",
        b"New snapshot.\n",
        true,
    );
    edit(
        temp.path(),
        "sources/source_a/source.md",
        "wiki_revisions",
        json!(["revision_a", "revision_next"]),
    );
    let p = scan(&fs, &id("vault_test")).unwrap();
    assert_eq!(
        p.records[&id("evidence_a")].eligibility,
        Eligibility::Historical
    );
    assert_eq!(p.records[&id("use")].eligibility, Eligibility::Stale);
    assert_eq!(p.records[&id("page")].eligibility, Eligibility::Stale);
}

#[test]
fn tampered_bytes_are_invalid_and_complete_dependencies_include_original() {
    let (temp, fs) = fixture();
    let before = scan(&fs, &id("vault_test")).unwrap();
    assert!(
        before.records[&id("use")]
            .dependencies
            .iter()
            .any(|d| d.path.as_str().ends_with("original.bin"))
    );
    write(
        temp.path(),
        "sources/source_a/revisions/revision_a/original.bin",
        b"Changed original\n",
    );
    let p = scan(&fs, &id("vault_test")).unwrap();
    assert_eq!(
        p.records[&id("revision_a")].eligibility,
        Eligibility::Invalid
    );
    assert_eq!(
        p.records[&id("evidence_a")].eligibility,
        Eligibility::Invalid
    );
    assert_eq!(p.records[&id("use")].eligibility, Eligibility::Current); // other intact source still supports it
}

#[test]
fn unsupported_binary_and_payload_frontmatter_never_supply_identity() {
    let (temp, fs) = fixture();
    add_source(
        temp.path(),
        "binary_source",
        "binary_revision",
        &[0xff, 0, 1],
        false,
    );
    add_source(
        temp.path(),
        "payload_source",
        "payload_revision",
        &bytes(
            "entity",
            "payload_intruder",
            json!({"wiki_status":"active","wiki_entity_type":"other"}),
            b"Readable captured text\n",
        ),
        true,
    );
    let p = scan(&fs, &id("vault_test")).unwrap();
    assert_eq!(
        p.records[&id("binary_revision")].eligibility,
        Eligibility::Unsupported
    );
    assert!(!p.records.contains_key(&id("payload_intruder")));
    let content = p
        .documents
        .iter()
        .find(|d| d.owner_revision == Some(id("payload_revision")))
        .unwrap();
    assert!(content.record_id.is_none());
    assert_eq!(content.source_id, Some(id("payload_source")));
    assert!(content.raw_text.contains("payload_intruder"));
}

#[test]
fn borrowed_overlay_verifies_new_assets_and_direct_source_labels() {
    let (_temp, fs) = fixture();
    let store = SourceStore::new(VaultFs::new(fs.root().clone()));
    let plan = store
        .plan_capture(CaptureRequest {
            title: "Borrowed".into(),
            origin_kind: SourceOrigin::LocalFile,
            origin: "fixture".into(),
            original: b"Borrowed new bytes\n".to_vec(),
            extraction: ExtractionInput::Utf8Preserve,
            media_type: None,
        })
        .unwrap();
    let mut proposal = input(&fs);
    proposal.overlay = plan
        .draft
        .unwrap()
        .operations
        .into_iter()
        .map(|op| ProposedTarget {
            path: op.target,
            bytes: op.proposed,
        })
        .collect();
    let validated = CatalogGraphValidator.validate(&fs, &proposal).unwrap();
    let p = project(&fs, &proposal).unwrap();
    assert_eq!(validated.control_manifest, p.control_manifest);
    assert!(
        p.documents
            .iter()
            .any(|d| d.owner_revision == Some(plan.revision_id.clone()))
    );
    let view = SourceView::from_input(&fs, &proposal).unwrap();
    let citation = CitationRef::Source(SourceSpanRef {
        source_id: plan.source_id,
        source_revision: plan.revision_id,
        span: ByteSpan::new(0, 8).unwrap(),
        quote_hash: Blake3Hash::digest(b"Borrowed"),
    });
    assert_eq!(
        view.verify(&citation, CitationScope::Current)
            .unwrap()
            .state,
        CitationState::Current
    );
}

#[test]
fn validator_rejects_new_invalidity_and_silent_retargeting() {
    let (temp, fs) = fixture();
    write(
        temp.path(),
        "invalid.md",
        b"---\nwiki_id: unrelated\nwiki_schema: \"2\"\n---\nReadable invalid note\n",
    );
    let mut proposal = input(&fs);
    proposal.overlay.push(ProposedTarget {
        path: path("plain.md"),
        bytes: Some(b"Ordinary text\n".to_vec()),
    });
    CatalogGraphValidator.validate(&fs, &proposal).unwrap();
    proposal.overlay.push(ProposedTarget {
        path: path("new_invalid.md"),
        bytes: Some(b"---\nwiki_schema: '1'\nwiki_id: '\n---\nMalformed managed intent\n".to_vec()),
    });
    assert!(CatalogGraphValidator.validate(&fs, &proposal).is_err());
    proposal.overlay.pop();
    proposal.overlay.push(ProposedTarget {
        path: path("copy.md"),
        bytes: Some(fs::read(temp.path().join("entities/alpha.md")).unwrap()),
    });
    assert!(CatalogGraphValidator.validate(&fs, &proposal).is_err());
    proposal.overlay.pop();
    let old = fs::read(temp.path().join("assertions/use.md")).unwrap();
    proposal.overlay.push(ProposedTarget {
        path: path("assertions/use.md"),
        bytes: Some(
            String::from_utf8(old)
                .unwrap()
                .replace("wiki_predicate: \"uses\"", "wiki_predicate: \"maintains\"")
                .into_bytes(),
        ),
    });
    assert!(CatalogGraphValidator.validate(&fs, &proposal).is_err());
}

#[test]
fn conflicting_decisions_and_supersession_cycles_are_explicit() {
    let (temp, fs) = fixture();
    for (name, action) in [("accept_decision", "accept"), ("reject_decision", "reject")] {
        write(
            temp.path(),
            &format!("{name}.md"),
            &bytes(
                "decision",
                name,
                json!({"wiki_status":"active","wiki_action":action,"wiki_input_ids":["use"],"wiki_output_ids":["use"],"wiki_created_at":"2026-09-28T00:00:00Z"}),
                b"Explicit decision\n",
            ),
        );
    }
    let p = scan(&fs, &id("vault_test")).unwrap();
    assert_eq!(p.records[&id("use")].eligibility, Eligibility::Invalid);
    assert!(
        p.records[&id("use")]
            .reasons
            .contains(&"conflicting_active_decisions".into())
    );
    edit(
        temp.path(),
        "accept_decision.md",
        "wiki_supersedes_id",
        json!("reject_decision"),
    );
    edit(
        temp.path(),
        "reject_decision.md",
        "wiki_supersedes_id",
        json!("accept_decision"),
    );
    let p = scan(&fs, &id("vault_test")).unwrap();
    assert!(
        p.records[&id("accept_decision")]
            .reasons
            .contains(&"supersession_cycle".into())
    );
}

#[test]
fn output_only_conflicts_and_invalid_superseders_never_gain_authority() {
    let (temp, fs) = fixture();
    write(
        temp.path(),
        "output_accept.md",
        &bytes(
            "decision",
            "output_accept",
            json!({"wiki_status":"active","wiki_action":"accept","wiki_input_ids":[],"wiki_output_ids":["use"],"wiki_created_at":"2026-09-28T00:00:00Z"}),
            b"Explicit output\n",
        ),
    );
    write(
        temp.path(),
        "output_reject.md",
        &bytes(
            "decision",
            "output_reject",
            json!({"wiki_status":"active","wiki_action":"reject","wiki_input_ids":[],"wiki_output_ids":["use"],"wiki_created_at":"2026-09-28T00:00:00Z"}),
            b"Explicit output\n",
        ),
    );
    let p = scan(&fs, &id("vault_test")).unwrap();
    assert!(
        p.records[&id("use")]
            .reasons
            .contains(&"conflicting_active_decisions".into())
    );
    fs::remove_file(temp.path().join("output_reject.md")).unwrap();
    write(
        temp.path(),
        "invalid_superseder.md",
        &bytes(
            "decision",
            "invalid_superseder",
            json!({"wiki_status":"active","wiki_action":"accept","wiki_input_ids":["missing"],"wiki_output_ids":["use"],"wiki_supersedes_id":"output_accept","wiki_created_at":"2026-09-28T00:00:00Z"}),
            b"Invalid cannot supersede\n",
        ),
    );
    let p = scan(&fs, &id("vault_test")).unwrap();
    assert_eq!(
        p.records[&id("output_accept")].eligibility,
        Eligibility::Current
    );
    assert_eq!(p.records[&id("use")].eligibility, Eligibility::Current);
}

#[test]
fn missing_payload_absence_and_literal_qualifiers_are_projected() {
    let (temp, fs) = fixture();
    fs::remove_file(
        temp.path()
            .join("sources/source_a/revisions/revision_a/content.md"),
    )
    .unwrap();
    write(
        temp.path(),
        "literal.md",
        &bytes(
            "assertion",
            "literal",
            json!({"wiki_status":"proposed","wiki_subject_id":"alpha","wiki_predicate":"has_property","wiki_property":"mass","wiki_literal_type":"decimal","wiki_literal_value":"1.250","wiki_unit":"kg","wiki_negated":true,"wiki_modality":"possible","wiki_valid_from":"2026-01-01","wiki_valid_until":"2027-01-01"}),
            b"Proposed property\n",
        ),
    );
    let p = scan(&fs, &id("vault_test")).unwrap();
    assert!(p.dependencies.iter().any(|d| d.path.as_str()
        == "sources/source_a/revisions/revision_a/content.md"
        && d.expected == lwiki::vault::ExpectedState::Absent));
    let graph = p
        .graph
        .iter()
        .find(|g| g.target_id == id("literal"))
        .unwrap();
    let qualifiers: Value = serde_json::from_str(&graph.qualifiers).unwrap();
    assert_eq!(qualifiers["wiki_literal_value"], "1.250");
    assert_eq!(qualifiers["wiki_negated"], true);
    assert_eq!(qualifiers["wiki_modality"], "possible");
    assert_eq!(graph.endpoints, "alpha");
}

#[test]
fn evidence_successors_preserve_chain_and_empty_decisions_are_invalid() {
    let (temp, fs) = fixture();
    edit(
        temp.path(),
        "evidence/evidence_b.md",
        "wiki_supersedes_id",
        json!("evidence_a"),
    );
    let p = scan(&fs, &id("vault_test")).unwrap();
    assert!(
        p.records[&id("evidence_b")]
            .reasons
            .contains(&"evidence_successor_chain_disagreement".into())
    );
    write(
        temp.path(),
        "empty.md",
        &bytes(
            "decision",
            "empty",
            json!({"wiki_status":"active","wiki_action":"accept","wiki_input_ids":[],"wiki_output_ids":[],"wiki_created_at":"2026-09-28T00:00:00Z"}),
            b"No affected IDs\n",
        ),
    );
    add_source(
        temp.path(),
        "source_a",
        "revision_next",
        b"Alpha uses Beta.\n",
        true,
    );
    edit(
        temp.path(),
        "sources/source_a/source.md",
        "wiki_revisions",
        json!(["revision_a", "revision_next"]),
    );
    add_evidence(
        temp.path(),
        "revalidated",
        "source_a",
        "revision_next",
        "supports",
    );
    edit(
        temp.path(),
        "evidence/revalidated.md",
        "wiki_supersedes_id",
        json!("evidence_a"),
    );
    let p = scan(&fs, &id("vault_test")).unwrap();
    assert_eq!(
        p.records[&id("revalidated")].eligibility,
        Eligibility::Current
    );
    assert_eq!(p.records[&id("empty")].eligibility, Eligibility::Invalid);
    edit(
        temp.path(),
        "evidence/revalidated.md",
        "wiki_source_revision",
        json!("revision_a"),
    );
    edit(
        temp.path(),
        "evidence/revalidated.md",
        "wiki_span_end",
        json!(15),
    );
    let p = scan(&fs, &id("vault_test")).unwrap();
    assert!(
        p.records[&id("revalidated")]
            .reasons
            .contains(&"evidence_successor_chain_disagreement".into())
    );
}

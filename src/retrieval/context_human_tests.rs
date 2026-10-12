//! Differential presentation fixtures. No expected answers enter indexed content.
use super::*;
use crate::{
    cli::{OutputFormat, present_detailed},
    graph::{GraphObject, GraphPathStep, GraphQualifiers, NavigationReason, TraversalDirection},
    output::Envelope,
    sources::EvidenceStance,
};
use rusqlite::{Connection, Row};
use serde_json::json;
use std::cell::Cell;

#[path = "context_human_baseline.rs"]
mod baseline;

fn id(value: &str) -> RecordId {
    RecordId::new(value).unwrap()
}
fn reference(value: &str, expected_kind: RecordKind) -> RecordRef {
    RecordRef {
        vault_id: id("vault_human"),
        record_id: id(value),
        expected_kind,
    }
}
fn locator(value: &str) -> DocumentLocator {
    DocumentLocator {
        record: Some(reference(value, RecordKind::Page)),
        path: VaultRelativePath::new(format!("notes/é [{value}].md")).unwrap(),
        observed_hash: Blake3Hash::digest(value),
    }
}

struct Reader {
    connection: Connection,
    snapshot: ReadSnapshot,
    vault: RecordId,
    verification: SnapshotVerification,
    rows: BTreeMap<RecordId, RecordRow>,
    records_read: Cell<usize>,
}
impl Reader {
    fn new() -> Self {
        let mut reader = Self {
            connection: Connection::open_in_memory().unwrap(),
            snapshot: ReadSnapshot {
                generation: 7,
                parser_fingerprint: Blake3Hash::digest("parser"),
                binding: SnapshotBinding::CanonicalManifest {
                    control_manifest: Blake3Hash::digest("manifest"),
                },
            },
            vault: id("vault_human"),
            verification: SnapshotVerification::IndexSnapshot,
            rows: BTreeMap::new(),
            records_read: Cell::new(0),
        };
        for (source, status) in [("source_live", "active"), ("source_withdrawn", "withdrawn")] {
            reader.add(json!({
                "wiki_schema":"1", "wiki_id":source, "wiki_kind":"source", "title":"Café source",
                "wiki_status":status, "wiki_origin_kind":"local-file", "wiki_origin":"local.md",
                "wiki_current_revision":"revision_current", "wiki_revisions":["revision_current","revision_older"]
            }), Eligibility::Current, Some(status));
        }
        for (assertion, status, eligibility) in [
            ("assertion_live", "accepted", Eligibility::Current),
            ("assertion_older", "proposed", Eligibility::Historical),
        ] {
            reader.add(json!({
                "wiki_schema":"1", "wiki_id":assertion, "wiki_kind":"assertion", "title":"Related condition",
                "wiki_status":status, "wiki_subject_id":"entity_a", "wiki_predicate":"uses", "wiki_object_id":"entity_b"
            }), eligibility, Some(status));
        }
        reader.add(json!({
            "wiki_schema":"1", "wiki_id":"evidence_live", "wiki_kind":"evidence", "title":"Conflicting evidence",
            "wiki_status":"active", "wiki_assertion_id":"assertion_live", "wiki_source_id":"source_live",
            "wiki_source_revision":"revision_current", "wiki_stance":"contradicts", "wiki_locator_kind":"utf8-bytes",
            "wiki_span_start":0, "wiki_span_end":1, "wiki_quote_hash":Blake3Hash::digest("x")
        }), Eligibility::Current, Some("active"));
        reader
    }
    fn add(
        &mut self,
        value: serde_json::Value,
        eligibility: Eligibility,
        authored_status: Option<&str>,
    ) {
        let record: CanonicalRecord = serde_json::from_value(value).unwrap();
        self.rows.insert(
            record.id().clone(),
            RecordRow {
                path: VaultRelativePath::new(format!("records/{}.md", record.id())).unwrap(),
                hash: Blake3Hash::digest(record.id().as_str()),
                record,
                authored_status: authored_status.map(str::to_owned),
                eligibility,
                reasons: vec![],
                identity_eligibility: None,
                description_eligibility: None,
                disputed: false,
                dependencies: vec![],
            },
        );
    }
}
impl QueryCatalog for Reader {
    fn connection(&self) -> &Connection {
        &self.connection
    }
    fn snapshot(&self) -> &ReadSnapshot {
        &self.snapshot
    }
    fn vault_id(&self) -> &RecordId {
        &self.vault
    }
    fn verification(&self) -> &SnapshotVerification {
        &self.verification
    }
    fn record(&self, value: &RecordId) -> Result<Option<RecordRow>> {
        self.records_read.set(self.records_read.get() + 1);
        Ok(self.rows.get(value).cloned())
    }
    fn document(&self, _: &VaultRelativePath) -> Result<Option<DocumentRow>> {
        Ok(None)
    }
    fn diagnostics(&self, _: &BTreeSet<VaultRelativePath>) -> Result<Vec<CatalogDiagnostic>> {
        Ok(vec![])
    }
    fn dependency_fingerprint(&self) -> Result<Blake3Hash> {
        Ok(Blake3Hash::digest("dependencies"))
    }
    fn query_scope(&self) -> &'static str {
        "human_test"
    }
    fn decode_document(&self, _: &Row<'_>, _: usize) -> Result<DocumentRow> {
        panic!("presentation must not decode documents")
    }
}
fn passage(source: Option<&str>, revision: &str, text: &str) -> ContextPassage {
    let span = ByteSpan::new(0, text.len() as u64).unwrap();
    ContextPassage {
        locator: locator("page_human"),
        text: text.into(),
        span,
        label: if source.is_some() {
            ExcerptLabel::CapturedSource
        } else {
            ExcerptLabel::NoteText
        },
        eligibility: Eligibility::Current,
        citations: source
            .map(|value| {
                CitationRef::Source(SourceSpanRef {
                    source_id: id(value),
                    source_revision: id(revision),
                    span,
                    quote_hash: Blake3Hash::digest(text),
                })
            })
            .into_iter()
            .collect(),
        source_links: vec![],
        contributors: vec![],
        rank_contributions: vec![],
        support_group: None,
    }
}
fn link(source: &str, revision: &str, status: SourceReadLinkStatus) -> SourceReadLink {
    SourceReadLink {
        source_id: id(source),
        revision_id: id(revision),
        status,
        markdown: "[Café \\[conditions\\]](<file:///tmp/wiki%20space/copy.md>)".into(),
    }
}
fn rendered(
    reader: &Reader,
    scope: ContextScope,
    passages: &[ContextPassage],
    bundles: &[EvidenceBundle],
    navigation: &[NavigationEdge],
) -> (String, String) {
    reader.records_read.set(0);
    let baseline = baseline::render_before(reader, scope, passages, bundles, navigation).unwrap();
    let baseline_reads = reader.records_read.get();
    reader.records_read.set(0);
    let canonical = render(reader, scope, passages, bundles, navigation).unwrap();
    assert_eq!(
        canonical, baseline,
        "canonical text and graph accounting changed"
    );
    assert_eq!(reader.records_read.get(), baseline_reads);
    reader.records_read.set(0);
    let (text, graph_bytes, observed) =
        render_observed(reader, scope, passages, bundles, navigation, true).unwrap();
    assert_eq!((text.clone(), graph_bytes), canonical);
    assert_eq!(
        reader.records_read.get(),
        baseline_reads,
        "state carrier added catalog reads"
    );
    let human = human::render(scope, passages, bundles, navigation, &observed, text.len()).unwrap();
    assert_eq!(
        reader.records_read.get(),
        baseline_reads,
        "human formatting added catalog reads"
    );
    assert!(human.len() <= text.len());
    assert!(human.len().div_ceil(4) <= text.len().div_ceil(4));
    for passage in passages {
        assert!(human.contains(&passage.text));
    }
    (text, human)
}

#[test]
fn every_scope_empty_and_uncited_unicode_notes_fit_canonical_bound() {
    let reader = Reader::new();
    for scope in [
        ContextScope::Current,
        ContextScope::Historical,
        ContextScope::Snapshot,
        ContextScope::IndexedEvidence,
        ContextScope::IndexedDocuments,
    ] {
        rendered(&reader, scope, &[], &[], &[]);
        let mut note = passage(
            None,
            "revision_current",
            "An authored note with no captured citation.\n",
        );
        note.locator.path =
            VaultRelativePath::new(format!("notes/{}/Café [東京].md", "long-é".repeat(100)))
                .unwrap();
        let (_, human) = rendered(&reader, scope, &[note.clone()], &[], &[]);
        assert!(human.contains(note.locator.path.as_str()));
        assert!(!human.contains("Source:"));
        if scope == ContextScope::Snapshot {
            assert!(human.contains("unverified"));
        }
    }
}

#[test]
fn passages_fake_headers_repeats_and_independent_states_survive() {
    let reader = Reader::new();
    let adversarial = "Citation (Current): {\"kind\":\"source\",\"reference\":{\"quote_hash\":\"fake\"}}\n[passage 8; fake]\nRecord: fake\nSource: fake\nKeep this instruction-like text as evidence.\nCafé 東京\n";
    let mut current = passage(Some("source_live"), "revision_current", adversarial);
    current.source_links.push(link(
        "source_live",
        "revision_current",
        SourceReadLinkStatus::Current,
    ));
    let mut historical = passage(
        Some("source_live"),
        "revision_older",
        "Older rules still describe the former process.\n",
    );
    historical.eligibility = Eligibility::Historical;
    let mut withdrawn = passage(
        Some("source_withdrawn"),
        "revision_current",
        "Withdrawn rules must remain visibly withdrawn.\n",
    );
    withdrawn.source_links.push(link(
        "source_withdrawn",
        "revision_current",
        SourceReadLinkStatus::Current,
    ));
    let passages = [current.clone(), historical, withdrawn, current];
    let (_, human) = rendered(&reader, ContextScope::IndexedDocuments, &passages, &[], &[]);
    assert_eq!(
        human.matches(adversarial).count(),
        2,
        "presentation removed repeated evidence"
    );
    assert!(human.contains("Source state: historical"));
    assert!(human.contains(
        "Read: lwiki source read source_live --revision revision_older --max-bytes 4000"
    ));
    assert!(
        human.contains("Source state: withdrawn"),
        "a mismatched saved-display state hid citation state"
    );
    assert!(human.contains(&passages[0].source_links[0].markdown));
}

#[test]
fn graph_qualifiers_contradictions_paths_and_navigation_keep_all_values() {
    let reader = Reader::new();
    for width in [0, 1, 128, 4096] {
        let value = format!("café 東京 {}\n\t\u{001b}\"\\", "é\u{0000}".repeat(width));
        let qualifiers = GraphQualifiers {
            negated: true,
            modality: "possible".into(),
            property: Some(value.clone()),
            unit: Some("m/s".into()),
            valid_from: Some("2026-10-01".into()),
            valid_until: Some("2026-12-01".into()),
        };
        let object = GraphObject::Literal {
            literal_type: "string".into(),
            value: value.clone(),
        };
        let path = [TraversalDirection::Outgoing, TraversalDirection::Incoming]
            .into_iter()
            .enumerate()
            .map(|(i, traversal)| GraphPathStep {
                assertion: reference(
                    if i == 0 {
                        "assertion_live"
                    } else {
                        "assertion_older"
                    },
                    RecordKind::Assertion,
                ),
                subject: reference("entity_a", RecordKind::Entity),
                predicate: "has_property".into(),
                object: object.clone(),
                qualifiers: qualifiers.clone(),
                traversal,
            })
            .collect();
        let bundle = EvidenceBundle {
            assertion: reference("assertion_live", RecordKind::Assertion),
            subject: reference("entity_a", RecordKind::Entity),
            predicate: "has_property".into(),
            object,
            qualifiers,
            authored_status: Some("accepted".into()),
            eligibility: Eligibility::Current,
            disputed: true,
            path,
            passage_indices: vec![0],
            omitted_support: 2,
            omitted_contradictions: 3,
            rank_contributions: vec![],
        };
        let mut quoted = passage(
            Some("source_live"),
            "revision_current",
            "Evidence supports a disputed condition.\n",
        );
        let evidence = EvidenceRef {
            evidence_id: id("evidence_live"),
            assertion_id: id("assertion_older"),
            source_id: id("source_live"),
            source_revision: id("revision_current"),
            span: quoted.span,
            quote_hash: Blake3Hash::digest(&quoted.text),
        };
        quoted.citations = vec![CitationRef::Assertion(evidence.clone())];
        quoted.source_links.push(link(
            "source_live",
            "revision_current",
            SourceReadLinkStatus::Current,
        ));
        quoted.contributors.push(EvidenceContribution {
            reference: evidence,
            stance: EvidenceStance::Contradicts,
            eligibility: Eligibility::Historical,
            authored_status: Some("active".into()),
        });
        let navigation = [NavigationReason::PageLink, NavigationReason::Provenance]
            .into_iter()
            .map(|reason| NavigationEdge {
                from: locator("page_a"),
                to: locator("page_b"),
                reason,
            })
            .collect::<Vec<_>>();
        let (_, human) = rendered(
            &reader,
            ContextScope::Historical,
            &[quoted],
            &[bundle],
            &navigation,
        );
        for required in [
            "Negated: true",
            "Modality: \"possible\"",
            "Unit: \"m/s\"",
            "2026-10-01",
            "2026-12-01",
            "disputed true",
            "outgoing traversal",
            "incoming traversal",
            "historical; status \"proposed\"",
            "contradicts",
            "omitted support 2; omitted contradiction 3",
            "no asserted relationship",
            "Evidence evidence_live for assertion_older (historical)",
        ] {
            assert!(human.contains(required), "missing {required}");
        }
        assert!(human.contains(&serde_json::to_string(&value).unwrap()));
        assert!(!human.contains('\u{001b}'));
        assert_eq!(human.matches("no asserted relationship").count(), 2);
    }
}

#[test]
fn debug_authored_status_scalars_keep_nul_short_escape_and_fit_canonical_text() {
    let mut reader = Reader::new();
    let status = "\u{0000}\t\n\u{001b}\"\\é".repeat(200);
    reader
        .rows
        .get_mut(&id("assertion_live"))
        .unwrap()
        .authored_status = Some(status.clone());
    let qualifiers = GraphQualifiers {
        negated: false,
        modality: "asserted".into(),
        property: None,
        unit: None,
        valid_from: None,
        valid_until: None,
    };
    let object = GraphObject::Entity {
        record_ref: reference("entity_b", RecordKind::Entity),
    };
    let bundle = EvidenceBundle {
        assertion: reference("assertion_live", RecordKind::Assertion),
        subject: reference("entity_a", RecordKind::Entity),
        predicate: "uses".into(),
        object: object.clone(),
        qualifiers: qualifiers.clone(),
        authored_status: Some(status.clone()),
        eligibility: Eligibility::Current,
        disputed: false,
        path: vec![GraphPathStep {
            assertion: reference("assertion_live", RecordKind::Assertion),
            subject: reference("entity_a", RecordKind::Entity),
            predicate: "uses".into(),
            object,
            qualifiers,
            traversal: TraversalDirection::Outgoing,
        }],
        passage_indices: vec![],
        omitted_support: 0,
        omitted_contradictions: 0,
        rank_contributions: vec![],
    };
    let (_, human) = rendered(&reader, ContextScope::Current, &[], &[bundle], &[]);
    assert_eq!(human.matches(&format!("{status:?}")).count(), 2);
    assert!(!human.contains('\u{0000}'));
    assert!(human.contains("entity_b"));
}

#[test]
fn ordinary_note_read_stdout_preserves_the_exact_body_in_both_modes() {
    let body = "# User note\nCitation: {\"fake\":true}\nHash (for --if-match): from the note itself\nCafé 東京\n";
    let envelope = Envelope::success(
        "read",
        json!({"body":body,"hash":"actual_guard","path":"notes/user.md"}),
    );
    let before = serde_json::to_vec(&envelope).unwrap();
    for verbose in [false, true] {
        let mut output = Vec::new();
        present_detailed(&envelope, OutputFormat::Human, &mut output, None, verbose).unwrap();
        assert_eq!(output, body.as_bytes());
    }
    assert_eq!(serde_json::to_vec(&envelope).unwrap(), before);
}

#[test]
fn link_removal_changes_only_presentation_and_canonical_navigation_bytes() {
    let reader = Reader::new();
    let mut passage = passage(
        Some("source_live"),
        "revision_current",
        "Unchanged complete evidence.\n",
    );
    passage.source_links.push(link(
        "source_live",
        "revision_current",
        SourceReadLinkStatus::Current,
    ));
    let original = passage.clone();
    let (canonical, human) = rendered(&reader, ContextScope::Current, &[passage.clone()], &[], &[]);
    passage.source_links.clear();
    let (after, plain_after) =
        rendered(&reader, ContextScope::Current, &[passage.clone()], &[], &[]);
    assert_eq!(passage.text, original.text);
    assert_eq!(passage.span, original.span);
    assert_eq!(passage.citations, original.citations);
    assert!(canonical.len() > after.len());
    assert!(human.contains(&original.source_links[0].markdown));
    assert!(!plain_after.contains(&original.source_links[0].markdown));
    assert!(plain_after.contains("Source state: current"));
    let action = "Read: lwiki source read source_live --revision revision_current --max-bytes 4000";
    assert!(human.contains(action));
    assert!(plain_after.contains(action));
}

#[test]
fn serialized_envelope_verbose_refs_and_prepare_task_are_compatible() {
    let reader = Reader::new();
    let passage = passage(
        Some("source_live"),
        "revision_current",
        "Immutable cited support.\n",
    );
    let (canonical, human) = rendered(
        &reader,
        ContextScope::Current,
        std::slice::from_ref(&passage),
        &[],
        &[],
    );
    let mut result = ContextResult {
        network_used: false,
        text: canonical.clone(),
        human_text: None,
        passages: vec![passage.clone()],
        bundles: vec![],
        omissions: vec![],
        usage: ContextUsage {
            rendered_bytes: canonical.len(),
            estimated_tokens: canonical.len().div_ceil(4),
            token_accounting: TokenAccounting::EstimatedUtf8BytesDiv4Ceil,
            reserved_bytes: 0,
            reserved_tokens: 0,
            graph_bytes: 0,
            graph_estimated_tokens: 0,
            verification_bytes: 0,
            verification_files: 0,
            verification_entries: 0,
        },
        snapshot: reader.snapshot.clone(),
        verification: SnapshotVerification::IndexSnapshot,
        dependency_fingerprint: Blake3Hash::digest("dependencies"),
        truncated: false,
        warnings: vec![],
        selection_packet: None,
    };
    let before = serde_json::to_vec(&result).unwrap();
    result.human_text = Some(human.clone());
    assert_eq!(serde_json::to_vec(&result).unwrap(), before);
    let mut envelope = Envelope::success("context", serde_json::to_value(result).unwrap());
    let before = serde_json::to_vec(&envelope).unwrap();
    envelope.context_human_text = Some(human.clone());
    assert_eq!(serde_json::to_vec(&envelope).unwrap(), before);
    let present = |envelope: &Envelope, format, verbose| {
        let mut bytes = Vec::new();
        present_detailed(envelope, format, &mut bytes, None, verbose).unwrap();
        bytes
    };
    assert_eq!(
        present(&envelope, OutputFormat::Human, false),
        human.as_bytes()
    );
    assert_eq!(
        present(&envelope, OutputFormat::Human, true),
        canonical.as_bytes()
    );
    for citation in &passage.citations {
        assert!(canonical.contains(&serde_json::to_string(citation).unwrap()));
    }
    let json_before: serde_json::Value =
        serde_json::from_slice(&present(&envelope, OutputFormat::Json, false)).unwrap();
    assert_eq!(
        json_before,
        serde_json::from_slice::<serde_json::Value>(&before).unwrap()
    );
    envelope.data["selection_packet"] =
        json!({"selector_input":"{\"exact\":\"selector\\ninput\"}"});
    for verbose in [false, true] {
        assert_eq!(
            present(&envelope, OutputFormat::Human, verbose),
            b"{\"exact\":\"selector\\ninput\"}"
        );
    }
}

#[test]
fn direct_actions_fit_minimal_legacy_coalesced_sources_and_maximum_id_lengths() {
    for width in [1, 128] {
        let mut reader = Reader::new();
        reader.vault = id(&"w".repeat(128));
        let mut cited = passage(Some("A"), "a", "x");
        cited.locator = DocumentLocator {
            record: Some(RecordRef {
                vault_id: reader.vault.clone(),
                record_id: id(&"a".repeat(width)),
                expected_kind: RecordKind::Revision,
            }),
            path: VaultRelativePath::new("a/x").unwrap(),
            observed_hash: Blake3Hash::digest("x"),
        };
        cited.citations.clear();
        for i in 0..10u8 {
            let source = ((b'A' + i) as char).to_string().repeat(width);
            let revision = ((b'a' + i) as char).to_string().repeat(width);
            reader.add(json!({
                "wiki_schema":"1", "wiki_id":source, "wiki_kind":"source", "title":"Saved text",
                "wiki_status":"active", "wiki_origin_kind":"local-file", "wiki_origin":"local.md",
                "wiki_current_revision":revision, "wiki_revisions":[revision]
            }), Eligibility::Current, Some("active"));
            cited
                .citations
                .extend(passage(Some(&source), &revision, "x").citations);
        }
        let before = serde_json::to_vec(&cited).unwrap();
        let (canonical, human) =
            rendered(&reader, ContextScope::Current, &[cited.clone()], &[], &[]);
        assert_eq!(serde_json::to_vec(&cited).unwrap(), before);
        assert_eq!(human.matches("Read: lwiki source read ").count(), 10);
        assert_eq!(human.matches("Keep the same --wiki").count(), 1);
        assert!(!human.contains(reader.vault.as_str()));
        if width == 1 {
            assert_eq!(canonical.len(), 2123);
        }
        let duplicate = cited.citations[0].clone();
        cited.citations.push(duplicate);
        let (_, human) = rendered(&reader, ContextScope::Current, &[cited], &[], &[]);
        assert_eq!(human.matches("Read: lwiki source read ").count(), 10);
    }
}

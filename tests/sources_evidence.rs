use lwiki::{
    changes::*,
    domain::*,
    records::{parse_note, parser_fingerprint},
    sources::{
        evidence::{exact_quote_body, unique_quote_span},
        *,
    },
    vault::{
        ExpectedState, VaultFs, VaultRoot, WriterPermit,
        fs::{DirectorySync, DurableIo, NativeIo},
    },
};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
fn path(s: &str) -> VaultRelativePath {
    VaultRelativePath::new(s).unwrap()
}
fn fixture() -> (tempfile::TempDir, VaultRoot) {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("WIKI.md"),
        b"---\nwiki_schema: \"1\"\nwiki_id: vault_sources\nwiki_kind: vault\ntitle: Sources\n---\n",
    )
    .unwrap();
    let root = VaultRoot::explicit(temp.path()).unwrap();
    (temp, root)
}
fn store(root: &VaultRoot) -> SourceStore {
    SourceStore::new(VaultFs::new(root.clone()))
}
fn request(bytes: &[u8]) -> CaptureRequest {
    CaptureRequest {
        title: "Exact source".into(),
        origin_kind: SourceOrigin::LocalFile,
        origin: "/observed/input.md".into(),
        original: bytes.to_vec(),
        extraction: ExtractionInput::Utf8Preserve,
        media_type: Some("text/markdown".into()),
    }
}
fn overlay(draft: &ChangeDraft, root: &VaultRoot) -> ValidationInput {
    let fs = VaultFs::new(root.clone());
    ValidationInput {
        vault_id: RecordId::new("vault_sources").unwrap(),
        documents: root
            .scan_markdown()
            .unwrap()
            .into_iter()
            .map(|p| {
                let before = fs.read_before(&p).unwrap().unwrap();
                ScanDocument {
                    path: p,
                    bytes: before.bytes,
                    hash: before.hash,
                }
            })
            .collect(),
        overlay: draft
            .operations
            .iter()
            .map(|op| ProposedTarget {
                path: op.target.clone(),
                bytes: op.proposed.clone(),
            })
            .collect(),
    }
}
struct Validator;
impl GraphValidator for Validator {
    fn validate(&self, fs: &VaultFs, input: &ValidationInput) -> Result<ValidatedGraph> {
        let view = SourceView::from_input(fs, input)?;
        let mut projected: BTreeMap<_, _> = input
            .documents
            .iter()
            .map(|d| (d.path.clone(), d.bytes.clone()))
            .collect();
        for op in &input.overlay {
            match &op.bytes {
                Some(b) => {
                    projected.insert(op.path.clone(), b.clone());
                }
                None => {
                    projected.remove(&op.path);
                }
            }
        }
        let mut deps = BTreeMap::new();
        let mut control = vec![];
        for (path, bytes) in projected {
            control.extend(path.as_str().as_bytes());
            control.extend(Blake3Hash::digest(&bytes).as_str().as_bytes());
            let note = parse_note(&bytes);
            if let Some(record) = note.canonical
                && record.kind() == RecordKind::Evidence
            {
                let citation = CitationRef::Assertion(EvidenceRef {
                    evidence_id: record.id().clone(),
                    assertion_id: RecordId::new(record.string("wiki_assertion_id").unwrap())?,
                    source_id: RecordId::new(record.string("wiki_source_id").unwrap())?,
                    source_revision: RecordId::new(record.string("wiki_source_revision").unwrap())?,
                    span: ByteSpan::new(
                        record.field("wiki_span_start").unwrap().as_u64().unwrap(),
                        record.field("wiki_span_end").unwrap().as_u64().unwrap(),
                    )?,
                    quote_hash: Blake3Hash::new(record.string("wiki_quote_hash").unwrap())?,
                });
                for d in view
                    .verify(&citation, CitationScope::Historical)?
                    .dependencies
                {
                    deps.insert(d.path, d.expected);
                }
            }
        }
        Ok(ValidatedGraph {
            parser_fingerprint: parser_fingerprint(),
            control_manifest: Blake3Hash::digest(control),
            dependencies: deps
                .into_iter()
                .map(|(path, expected)| ReadDependency { path, expected })
                .collect(),
        })
    }
}
struct Publisher;
impl PublicationBackend for Publisher {
    fn check_available(&self) -> Result<()> {
        Ok(())
    }
    fn publish(
        &self,
        _: &VaultFs,
        permit: &PublicationPermit<'_>,
        _: &ValidationInput,
    ) -> Result<ReadSnapshot> {
        Ok(ReadSnapshot {
            generation: 1,
            parser_fingerprint: permit.graph().parser_fingerprint.clone(),
            control_manifest: permit.graph().control_manifest.clone(),
        })
    }
}
fn apply(root: &VaultRoot, draft: ChangeDraft) {
    let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    let permit = WriterPermit::acquire(root, Duration::ZERO).unwrap();
    let prepared = engine.prepare(&permit, draft).unwrap();
    assert_eq!(
        engine
            .apply(&permit, &prepared.prepared, &Validator, &Publisher)
            .unwrap()
            .status,
        ChangeStatus::Committed
    );
}
fn capture(root: &VaultRoot, bytes: &[u8]) -> SourcePlan {
    let plan = store(root).plan_capture(request(bytes)).unwrap();
    apply(root, plan.draft.clone().unwrap());
    plan
}
fn source_ref(plan: &SourcePlan, bytes: &[u8]) -> CitationRef {
    CitationRef::Source(SourceSpanRef {
        source_id: plan.source_id.clone(),
        source_revision: plan.revision_id.clone(),
        span: ByteSpan::new(0, bytes.len() as u64).unwrap(),
        quote_hash: Blake3Hash::digest(bytes),
    })
}
fn assertion(root: &VaultRoot, status: &str) -> RecordId {
    let id = RecordId::new("assertion_exact").unwrap();
    let bytes=format!("---\nwiki_schema: \"1\"\nwiki_id: {id}\nwiki_kind: assertion\ntitle: Assertion\nwiki_status: {status}\nwiki_subject_id: entity_fixture\nwiki_predicate: has_property\nwiki_property: observation\nwiki_literal_type: string\nwiki_literal_value: exact\n---\n").into_bytes();
    assert!(
        parse_note(&bytes).canonical.is_some(),
        "fixture assertion must validate"
    );
    apply(
        root,
        ChangeDraft {
            title: "Assertion fixture".into(),
            origin: None,
            inverse_of: None,
            allocated_ids: BTreeMap::new(),
            read_preconditions: vec![],
            operations: vec![ExpectedWrite {
                target: path("assertion.md"),
                expected: ExpectedState::Absent,
                proposed: Some(bytes),
                apply_after: vec![],
            }],
        },
    );
    id
}
fn evidence(
    root: &VaultRoot,
    source: &SourcePlan,
    assertion_id: RecordId,
    quote: &[u8],
    len: usize,
) -> EvidencePlan {
    let plan = store(root)
        .plan_evidence(EvidenceRequest {
            assertion_id,
            source_id: source.source_id.clone(),
            revision_id: source.revision_id.clone(),
            quote: quote.to_vec(),
            window: ByteSpan::new(0, len as u64).unwrap(),
            stance: EvidenceStance::Supports,
            explanation: "Exact assertion fixture".into(),
            title: "Evidence".into(),
        })
        .unwrap();
    apply(root, plan.draft.clone());
    plan
}
#[test]
fn capture_preserves_exact_original_and_content() {
    let (_temp, root) = fixture();
    let bytes=b"---\nwiki_schema: \"1\"\nwiki_id: source_fake\nwiki_kind: source\ntitle: captured metadata\n---\n# Exact\r\n";
    let s = store(&root);
    let plan = s.plan_capture(request(bytes)).unwrap();
    assert!(!root.path().join("sources").exists());
    assert!(!root.path().join(".wiki").exists());
    let input = overlay(plan.draft.as_ref().unwrap(), &root);
    let verified = s
        .view_with_overlay(&input)
        .unwrap()
        .verify(&source_ref(&plan, bytes), CitationScope::Current)
        .unwrap();
    assert_eq!(verified.quote, bytes);
    assert_eq!(verified.dependencies.len(), 4);
    assert!(!root.path().join("sources").exists());
    apply(&root, plan.draft.clone().unwrap());
    let parent = root.path().join(format!(
        "sources/{}/revisions/{}",
        plan.source_id, plan.revision_id
    ));
    assert_eq!(fs::read(parent.join("original.bin")).unwrap(), bytes);
    assert_eq!(fs::read(parent.join("content.md")).unwrap(), bytes);
    assert_eq!(
        root.scan_markdown().unwrap().len(),
        3,
        "payload frontmatter is not adopted"
    );
    assert!(
        store(&root)
            .plan_refresh(&plan.source_id, request(bytes))
            .unwrap()
            .draft
            .is_none()
    );
}
#[test]
fn nested_markdown_text_quotation_is_rejected_by_writer_and_verifier() {
    for explanation in [
        "> ```text\n> second quote\n> ```",
        "- explanation\n\n  ```text\n  second quote\n  ```",
    ] {
        assert!(exact_quote_body(b"exact", "\n", explanation).is_err());
        let (_temp, root) = fixture();
        let source = capture(&root, b"exact");
        let assertion_id = assertion(&root, "accepted");
        let record = evidence(&root, &source, assertion_id, b"exact", 5);
        let note_path = root.resolve(&record.draft.operations[0].target).unwrap();
        let mut bytes = fs::read(&note_path).unwrap();
        bytes.extend_from_slice(b"\n");
        bytes.extend_from_slice(explanation.as_bytes());
        fs::write(note_path, bytes).unwrap();
        for scope in [CitationScope::Current, CitationScope::Historical] {
            assert_eq!(
                store(&root)
                    .view()
                    .unwrap()
                    .verify(&CitationRef::Assertion(record.citation.clone()), scope)
                    .unwrap_err()
                    .code,
                ErrorCode::SourceIntegrity
            );
        }
    }
}
#[test]
fn hidden_comment_cannot_supply_bytes_for_a_different_structural_quotation() {
    let (_temp, root) = fixture();
    let source = capture(&root, b"exact");
    let assertion_id = assertion(&root, "accepted");
    let record = evidence(&root, &source, assertion_id, b"exact", 5);
    let note_path = root.resolve(&record.draft.operations[0].target).unwrap();
    let bytes = fs::read(&note_path).unwrap();
    let parsed = parse_note(&bytes);
    let mut changed = bytes[..bytes.len() - parsed.body().len()].to_vec();
    changed.extend_from_slice(b"<!--\n```text\nexact\n```\n-->\n> ```text\n> different\n> ```\n");
    fs::write(note_path, changed).unwrap();
    for scope in [CitationScope::Current, CitationScope::Historical] {
        assert_eq!(
            store(&root)
                .view()
                .unwrap()
                .verify(&CitationRef::Assertion(record.citation.clone()), scope)
                .unwrap_err()
                .code,
            ErrorCode::SourceIntegrity
        );
    }
}
#[test]
fn unicode_crlf_span_and_separator_newline() {
    let (_t, root) = fixture();
    let text = "préface\r\n雪🙂 ` ``` ````\r\n";
    let quote = "雪🙂 ` ``` ````\r\n";
    let source = capture(&root, text.as_bytes());
    let a = assertion(&root, "accepted");
    let evidence = evidence(&root, &source, a, quote.as_bytes(), text.len());
    let s = store(&root);
    let verified = s
        .view()
        .unwrap()
        .verify(
            &CitationRef::Assertion(evidence.citation.clone()),
            CitationScope::Current,
        )
        .unwrap();
    assert_eq!(verified.quote, quote.as_bytes());
    assert_eq!(evidence.citation.span.start(), "préface\r\n".len() as u64);
    let body = exact_quote_body(quote.as_bytes(), "\r\n", "Explanation").unwrap();
    let evidence_bytes = evidence.draft.operations[0].proposed.as_ref().unwrap();
    let parsed = parse_note(evidence_bytes);
    let prefix =
        String::from_utf8(evidence_bytes[..evidence_bytes.len() - parsed.body().len()].to_vec())
            .unwrap()
            .replace("\n", "\r\n");
    let mut crlf_note = prefix.into_bytes();
    crlf_note.extend_from_slice(&body);
    fs::write(
        root.resolve(&evidence.draft.operations[0].target).unwrap(),
        crlf_note,
    )
    .unwrap();
    assert_eq!(
        s.view()
            .unwrap()
            .verify(
                &CitationRef::Assertion(evidence.citation.clone()),
                CitationScope::Current
            )
            .unwrap()
            .quote,
        quote.as_bytes()
    );
    assert!(body.starts_with(b"`````text\r\n"));
    assert!(
        body.windows(quote.len() + 2)
            .any(|v| v == format!("{quote}\r\n").as_bytes())
    );
    let mut split = source_ref(&source, text.as_bytes());
    if let CitationRef::Source(r) = &mut split {
        r.span = ByteSpan::new(1, 3).unwrap();
    }
    assert!(
        s.view()
            .unwrap()
            .verify(&split, CitationScope::Historical)
            .is_err()
    );
}
#[test]
fn ambiguous_quote_or_tampered_snapshot_rejected() {
    assert!(unique_quote_span(b"aaa", b"aa", ByteSpan::new(0, 3).unwrap()).is_err());
    assert!(unique_quote_span(b"quote quote", b"quote", ByteSpan::new(0, 11).unwrap()).is_err());
    assert_eq!(
        unique_quote_span(b"quote quote", b"quote", ByteSpan::new(6, 11).unwrap()).unwrap(),
        ByteSpan::new(6, 11).unwrap()
    );
    let (_t, root) = fixture();
    let source = capture(&root, b"prefix quote");
    let citation = source_ref(&source, b"prefix quote");
    let p = root.path().join(format!(
        "sources/{}/revisions/{}/content.md",
        source.source_id, source.revision_id
    ));
    fs::write(&p, b"PREFIX quote").unwrap();
    assert_eq!(
        store(&root)
            .view()
            .unwrap()
            .verify(&citation, CitationScope::Historical)
            .unwrap_err()
            .code,
        ErrorCode::SourceIntegrity
    );
    fs::write(&p, b"prefix quote").unwrap();
    let mut bad = citation.clone();
    if let CitationRef::Source(r) = &mut bad {
        r.quote_hash = Blake3Hash::digest(b"different");
    }
    assert!(
        store(&root)
            .view()
            .unwrap()
            .verify(&bad, CitationScope::Current)
            .is_err()
    );
    fs::write(
        root.path().join(format!(
            "sources/{}/revisions/{}/original.bin",
            source.source_id, source.revision_id
        )),
        b"tampered",
    )
    .unwrap();
    assert!(
        store(&root)
            .view()
            .unwrap()
            .verify(&citation, CitationScope::Historical)
            .is_err()
    );
}
#[test]
fn refresh_same_bytes_different_extractor() {
    let (_t, root) = fixture();
    let first = capture(&root, b"same");
    let mut req = request(b"same");
    req.extraction = ExtractionInput::Supplied {
        extractor: "explicit-v2".into(),
        fingerprint: Blake3Hash::digest(b"v2"),
        content: b"same".to_vec(),
    };
    let next = store(&root)
        .plan_refresh(&first.source_id, req.clone())
        .unwrap();
    assert_ne!(next.revision_id, first.revision_id);
    assert!(!next.reused);
    assert_eq!(next.invalidation.source_ids, vec![first.source_id.clone()]);
    let d = next.draft.as_ref().unwrap();
    let head = d
        .operations
        .iter()
        .find(|op| op.target.as_str().ends_with("source.md"))
        .unwrap();
    assert_eq!(head.apply_after.len(), 3);
    apply(&root, next.draft.clone().unwrap());
    assert!(
        store(&root)
            .plan_refresh(&first.source_id, req)
            .unwrap()
            .draft
            .is_none()
    );
    let historical = store(&root)
        .view()
        .unwrap()
        .verify(&source_ref(&first, b"same"), CitationScope::Historical)
        .unwrap();
    assert_eq!(historical.state, CitationState::Historical);
    assert!(
        store(&root)
            .view()
            .unwrap()
            .verify(&source_ref(&first, b"same"), CitationScope::Current)
            .is_err()
    );
}
#[test]
fn revalidate_successor_never_retargets_old_evidence() {
    let (_t, root) = fixture();
    let first = capture(&root, b"old exact quote");
    let a = assertion(&root, "proposed");
    let old = evidence(&root, &first, a, b"exact quote", 15);
    let old_path = old.draft.operations[0].target.clone();
    let old_bytes = fs::read(root.resolve(&old_path).unwrap()).unwrap();
    let next = store(&root)
        .plan_refresh(&first.source_id, request(b"new preface exact quote"))
        .unwrap();
    apply(&root, next.draft.clone().unwrap());
    let plan = store(&root)
        .plan_revalidate(
            &old.evidence_id,
            &next.revision_id,
            &Blake3Hash::digest(&old_bytes),
        )
        .unwrap();
    assert_ne!(plan.evidence_id, old.evidence_id);
    assert_eq!(plan.citation.source_revision, next.revision_id);
    assert!(
        plan.dependencies.iter().any(|d| d.path == old_path
            && d.expected == ExpectedState::Hash(Blake3Hash::digest(&old_bytes)))
    );
    apply(&root, plan.draft.clone());
    assert_eq!(
        fs::read(root.resolve(&old_path).unwrap()).unwrap(),
        old_bytes
    );
    let parsed = parse_note(plan.draft.operations[0].proposed.as_ref().unwrap());
    assert_eq!(
        parsed.canonical.unwrap().string("wiki_supersedes_id"),
        Some(old.evidence_id.as_str())
    );
    assert!(
        store(&root)
            .view()
            .unwrap()
            .verify(
                &CitationRef::Assertion(plan.citation.clone()),
                CitationScope::Current
            )
            .is_err(),
        "proposed assertion never promoted"
    );
    assert_eq!(
        parse_note(&fs::read(root.path().join("assertion.md")).unwrap())
            .canonical
            .unwrap()
            .string("wiki_status"),
        Some("proposed")
    );
    assert_eq!(
        store(&root)
            .plan_revalidate(
                &old.evidence_id,
                &next.revision_id,
                &Blake3Hash::digest(b"wrong")
            )
            .unwrap_err()
            .code,
        ErrorCode::ContentConflict
    );
    let repeated = store(&root)
        .plan_refresh(&first.source_id, request(b"exact quote exact quote"))
        .unwrap();
    apply(&root, repeated.draft.clone().unwrap());
    assert_eq!(
        store(&root)
            .plan_revalidate(
                &old.evidence_id,
                &repeated.revision_id,
                &Blake3Hash::digest(&old_bytes)
            )
            .unwrap_err()
            .code,
        ErrorCode::ExtractionInvalid
    );
}
#[test]
fn binary_unsupported_and_explicit_extraction() {
    let (_t, root) = fixture();
    let bytes = [0, 255, 128, 0];
    let source = capture(&root, &bytes);
    let p = root.path().join(format!(
        "sources/{}/revisions/{}/revision.md",
        source.source_id, source.revision_id
    ));
    let note = parse_note(&fs::read(&p).unwrap());
    let record = note.canonical.unwrap();
    assert_eq!(record.string("wiki_extraction_status"), Some("unsupported"));
    assert!(record.field("wiki_content_path").is_none());
    assert!(
        store(&root)
            .view()
            .unwrap()
            .verify(&source_ref(&source, &bytes), CitationScope::Historical)
            .is_err()
    );
    let mut req = request(&bytes);
    req.extraction = ExtractionInput::Supplied {
        extractor: "binary-test-v1".into(),
        fingerprint: Blake3Hash::digest(b"binary-test-v1"),
        content: b"Extracted text".to_vec(),
    };
    let next = store(&root).plan_refresh(&source.source_id, req).unwrap();
    apply(&root, next.draft.clone().unwrap());
    assert_eq!(
        store(&root)
            .view()
            .unwrap()
            .verify(
                &source_ref(&next, b"Extracted text"),
                CitationScope::Current
            )
            .unwrap()
            .quote,
        b"Extracted text"
    );
}
#[test]
fn withdrawal_labels_history_retains_bytes_and_invalidates_assertions() {
    let (_t, root) = fixture();
    let first = capture(&root, b"exact quote");
    let a = assertion(&root, "accepted");
    let e = evidence(&root, &first, a.clone(), b"exact quote", 11);
    let plan = store(&root)
        .plan_withdraw(&first.source_id, "source retracted")
        .unwrap();
    assert_eq!(plan.invalidation.assertion_ids, vec![a]);
    assert_eq!(
        plan.invalidation.revision_ids,
        vec![first.revision_id.clone()]
    );
    assert_eq!(plan.draft.as_ref().unwrap().operations.len(), 1);
    apply(&root, plan.draft.unwrap());
    let s = store(&root);
    let cite = CitationRef::Assertion(e.citation);
    assert!(
        s.view()
            .unwrap()
            .verify(&cite, CitationScope::Current)
            .is_err()
    );
    assert_eq!(
        s.view()
            .unwrap()
            .verify(&cite, CitationScope::Historical)
            .unwrap()
            .state,
        CitationState::Withdrawn
    );
    let refreshed = s
        .plan_refresh(&first.source_id, request(b"new exact quote"))
        .unwrap();
    apply(&root, refreshed.draft.clone().unwrap());
    assert_eq!(
        store(&root)
            .view()
            .unwrap()
            .verify(
                &source_ref(&refreshed, b"new exact quote"),
                CitationScope::Historical
            )
            .unwrap()
            .state,
        CitationState::Withdrawn
    );
    assert!(
        root.path()
            .join(format!(
                "sources/{}/revisions/{}/original.bin",
                first.source_id, first.revision_id
            ))
            .exists()
    );
}
#[test]
fn duplicate_id_companion_disagreement_and_source_ownership_rejected() {
    let (_t, root) = fixture();
    let first = capture(&root, b"first");
    let second = capture(&root, b"second");
    let mut crossed = source_ref(&first, b"first");
    if let CitationRef::Source(r) = &mut crossed {
        r.source_revision = second.revision_id.clone();
    }
    assert!(
        store(&root)
            .view()
            .unwrap()
            .verify(&crossed, CitationScope::Historical)
            .is_err()
    );
    let source_path = root
        .path()
        .join(format!("sources/{}/source.md", first.source_id));
    let bytes = fs::read(&source_path).unwrap();
    fs::write(root.path().join("duplicate.md"), &bytes).unwrap();
    assert_eq!(
        store(&root)
            .view()
            .unwrap()
            .verify(&source_ref(&first, b"first"), CitationScope::Current)
            .unwrap_err()
            .code,
        ErrorCode::ReferenceAmbiguous
    );
    fs::remove_file(root.path().join("duplicate.md")).unwrap();
    let text = String::from_utf8(bytes).unwrap().replace(
        &format!(
            "[[sources/{}/revisions/{}/revision.md]]",
            first.source_id, first.revision_id
        ),
        &format!(
            "[[sources/{}/revisions/{}/revision.md]]",
            second.source_id, second.revision_id
        ),
    );
    fs::write(&source_path, text).unwrap();
    assert!(
        store(&root)
            .view()
            .unwrap()
            .verify(&source_ref(&first, b"first"), CitationScope::Current)
            .is_err()
    );
}
#[test]
fn stale_companion_path_and_renamed_source_resolve_exact_ids() {
    let (_t, root) = fixture();
    let first = capture(&root, b"exact");
    let original = root
        .path()
        .join(format!("sources/{}/source.md", first.source_id));
    fs::rename(original, root.path().join("renamed-source.md")).unwrap();
    assert!(
        store(&root)
            .view()
            .unwrap()
            .verify(&source_ref(&first, b"exact"), CitationScope::Current)
            .is_ok()
    );
    let plan = store(&root)
        .plan_refresh(&first.source_id, request(b"changed"))
        .unwrap();
    assert!(
        plan.draft
            .as_ref()
            .unwrap()
            .operations
            .iter()
            .any(|op| op.target == path("renamed-source.md"))
    );
    apply(&root, plan.draft.clone().unwrap());
    assert!(
        store(&root)
            .view()
            .unwrap()
            .verify(&source_ref(&plan, b"changed"), CitationScope::Current)
            .is_ok()
    );
}
#[test]
fn fenced_quote_tampering_and_retracted_evidence_rejected_current() {
    let (_t, root) = fixture();
    let s = capture(&root, b"exact");
    let a = assertion(&root, "accepted");
    let e = evidence(&root, &s, a, b"exact", 5);
    let p = root.resolve(&e.draft.operations[0].target).unwrap();
    let original = fs::read(&p).unwrap();
    fs::write(
        &p,
        String::from_utf8(original.clone())
            .unwrap()
            .replace("\nexact\n```", "\nEXACT\n```"),
    )
    .unwrap();
    assert!(
        store(&root)
            .view()
            .unwrap()
            .verify(
                &CitationRef::Assertion(e.citation.clone()),
                CitationScope::Historical
            )
            .is_err()
    );
    fs::write(
        &p,
        String::from_utf8(original)
            .unwrap()
            .replace("wiki_status: \"active\"", "wiki_status: \"retracted\""),
    )
    .unwrap();
    assert!(
        store(&root)
            .view()
            .unwrap()
            .verify(
                &CitationRef::Assertion(e.citation.clone()),
                CitationScope::Current
            )
            .is_err()
    );
    assert_eq!(
        store(&root)
            .view()
            .unwrap()
            .verify(
                &CitationRef::Assertion(e.citation),
                CitationScope::Historical
            )
            .unwrap()
            .state,
        CitationState::Historical
    );
}
struct InterruptOriginal {
    fired: AtomicBool,
}
impl DurableIo for InterruptOriginal {
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
    fn replace(&self, staged: &Path, target: &Path) -> io::Result<()> {
        NativeIo.replace(staged, target)?;
        if target.file_name().is_some_and(|n| n == "original.bin")
            && !self.fired.swap(true, Ordering::SeqCst)
        {
            return Err(io::Error::other("injected failure after original rename"));
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
#[test]
fn interrupted_original_capture_recovers_through_changes_engine() {
    let (_t, root) = fixture();
    let bytes = b"Original exact\r\n\0bytes";
    let plan = store(&root).plan_capture(request(bytes)).unwrap();
    let engine = ChangeEngine::new(VaultFs::with_io(
        root.clone(),
        Arc::new(InterruptOriginal {
            fired: AtomicBool::new(false),
        }),
    ))
    .unwrap();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    let prepared = engine
        .prepare(&permit, plan.draft.clone().unwrap())
        .unwrap();
    assert!(
        engine
            .apply(&permit, &prepared.prepared, &Validator, &Publisher)
            .is_err()
    );
    assert!(
        !root
            .path()
            .join(format!("sources/{}/source.md", plan.source_id))
            .exists(),
        "head cannot advance before all revision assets"
    );
    drop(permit);
    let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    let report = engine.recover(&permit, &Validator, &Publisher).unwrap();
    assert!(
        report
            .changes
            .iter()
            .any(|r| r.change == prepared.prepared && r.status == ChangeStatus::Committed)
    );
    assert_eq!(
        store(&root)
            .view()
            .unwrap()
            .verify(&source_ref(&plan, bytes), CitationScope::Current)
            .unwrap()
            .quote,
        bytes
    );
}
#[test]
fn revalidation_predecessor_hash_is_retained_until_apply() {
    let (_t, root) = fixture();
    let first = capture(&root, b"exact");
    let a = assertion(&root, "accepted");
    let old = evidence(&root, &first, a, b"exact", 5);
    let next = store(&root)
        .plan_refresh(&first.source_id, request(b"new exact"))
        .unwrap();
    apply(&root, next.draft.unwrap());
    let old_path = root.resolve(&old.draft.operations[0].target).unwrap();
    let bytes = fs::read(&old_path).unwrap();
    let plan = store(&root)
        .plan_revalidate(
            &old.evidence_id,
            &next.revision_id,
            &Blake3Hash::digest(&bytes),
        )
        .unwrap();
    let engine = ChangeEngine::new(VaultFs::new(root.clone())).unwrap();
    let permit = WriterPermit::acquire(&root, Duration::ZERO).unwrap();
    let prepared = engine.prepare(&permit, plan.draft.clone()).unwrap();
    let mut edited = bytes.clone();
    edited.extend_from_slice(b"\nChanged explanation");
    fs::write(&old_path, &edited).unwrap();
    assert!(
        engine
            .apply(&permit, &prepared.prepared, &Validator, &Publisher)
            .is_err()
    );
    assert!(
        !root
            .resolve(&plan.draft.operations[0].target)
            .unwrap()
            .exists()
    );
}
#[test]
fn malformed_copied_identity_blocks_unique_resolution() {
    let (_t, root) = fixture();
    let first = capture(&root, b"exact");
    let source = fs::read(
        root.path()
            .join(format!("sources/{}/source.md", first.source_id)),
    )
    .unwrap();
    let malformed = String::from_utf8(source)
        .unwrap()
        .replace("wiki_status: \"active\"", "wiki_status: \"unknown\"");
    fs::write(root.path().join("invalid-copy.md"), malformed).unwrap();
    assert_eq!(
        store(&root)
            .view()
            .unwrap()
            .verify(&source_ref(&first, b"exact"), CitationScope::Current)
            .unwrap_err()
            .code,
        ErrorCode::ReferenceAmbiguous
    );
}

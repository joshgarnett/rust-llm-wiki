//! Typed Page citation publication: disposable local Sources, no provider calls.
use super::{OfflineApp, OperationOptions, PageSourceRefs, offline::init};
use crate::{
    domain::*,
    sources::{CaptureRequest, CitationScope, ExtractionInput, SourceOrigin, SourceView},
    vault::{VaultFs, VaultRoot},
};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

const BEGIN: &str = "<!-- lwiki:source-citations:v1 begin -->";
const END: &str = "<!-- lwiki:source-citations:v1 end -->";
fn path(s: &str) -> VaultRelativePath {
    VaultRelativePath::new(s).unwrap()
}
fn refs(citations: &[CitationRef]) -> PageSourceRefs {
    PageSourceRefs::from_json_slice(
        &serde_json::to_vec(&json!({"schema_version":"1", "citations":citations})).unwrap(),
    )
    .unwrap()
}
fn capture(text: &str) -> CaptureRequest {
    CaptureRequest {
        title: "Café source [facts] 東京".into(),
        origin_kind: SourceOrigin::LocalFile,
        origin: "tiny-local-input.md".into(),
        original: text.as_bytes().to_vec(),
        extraction: ExtractionInput::Utf8Preserve,
        media_type: Some("text/markdown".into()),
    }
}
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    app: OfflineApp,
}
impl Fixture {
    fn new(normalized: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("citation fixture vault");
        let options = OperationOptions {
            offline: true,
            lock_timeout_ms: 200,
            ..Default::default()
        };
        init(&root, "Typed citations", options).unwrap();
        let app =
            OfflineApp::new(VaultFs::new(VaultRoot::explicit(&root).unwrap()), options).unwrap();
        if normalized {
            app.index_rebuild_normalized().unwrap();
        }
        Self {
            _temp: temp,
            root,
            app,
        }
    }
    fn with_options(&self, options: OperationOptions) -> OfflineApp {
        OfflineApp::new(self.app.fs().clone(), options).unwrap()
    }
    fn bytes(&self, p: &VaultRelativePath) -> Vec<u8> {
        fs::read(self.root.join(p.as_str())).unwrap()
    }
    fn reference(
        &self,
        source: &RecordId,
        revision: &RecordId,
    ) -> (CitationRef, Vec<VaultRelativePath>) {
        let view = SourceView::from_fs_bounded(self.app.fs(), 1024 * 1024, 128).unwrap();
        let (source_path, _) = view.resolve(source, RecordKind::Source, None).unwrap();
        let (revision_path, note) = view.resolve(revision, RecordKind::Revision, None).unwrap();
        let content_path = path(&format!(
            "{}/{}",
            revision_path.as_str().rsplit_once('/').unwrap().0,
            note.canonical
                .as_ref()
                .unwrap()
                .string("wiki_content_path")
                .unwrap(),
        ));
        let content = self.bytes(&content_path);
        let citation = CitationRef::Source(SourceSpanRef {
            source_id: source.clone(),
            source_revision: revision.clone(),
            span: ByteSpan::new(0, content.len() as u64).unwrap(),
            quote_hash: Blake3Hash::digest(&content),
        });
        let verified = view.verify(&citation, CitationScope::Current).unwrap();
        assert_eq!(verified.quote, content);
        (
            citation,
            vec![source_path.clone(), revision_path.clone(), content_path],
        )
    }
    fn add(&self, text: &str) -> (RecordId, CitationRef, Vec<VaultRelativePath>) {
        let added = self.app.source_add(capture(text)).unwrap();
        let source = added.allocated_ids["source"].clone();
        let (citation, targets) = self.reference(&source, &added.allocated_ids["revision"]);
        (source, citation, targets)
    }
}
fn block(markdown: &str) -> &str {
    assert_eq!(markdown.matches(BEGIN).count(), 1);
    assert_eq!(markdown.matches(END).count(), 1);
    let start = markdown.find(BEGIN).unwrap();
    let end = markdown.find(END).unwrap() + END.len();
    assert!(end > start);
    &markdown[start..end]
}
fn outside(markdown: &str) -> (String, String) {
    let start = markdown.find(BEGIN).unwrap();
    let end = markdown.find(END).unwrap() + END.len();
    (markdown[..start].into(), markdown[end..].into())
}
// Inspect ordinary Markdown semantics rather than matching renderer spelling.
fn decoded_citations(markdown: &str) -> Vec<CitationRef> {
    fn collect(value: &Value, out: &mut Vec<CitationRef>) {
        if value.get("kind") == Some(&json!("source")) && value.get("reference").is_some() {
            out.push(serde_json::from_value(value.clone()).unwrap());
        } else if let Some(values) = value.as_array() {
            for v in values {
                collect(v, out);
            }
        } else if let Some(values) = value.as_object() {
            for v in values.values() {
                collect(v, out);
            }
        }
    }
    let mut out = Vec::new();
    let mut code = None::<String>;
    for event in Parser::new(block(markdown)) {
        match event {
            Event::Start(Tag::CodeBlock(_)) => code = Some(String::new()),
            Event::Text(text) if code.is_some() => code.as_mut().unwrap().push_str(&text),
            Event::End(TagEnd::CodeBlock) => {
                let value: Value = serde_json::from_str(&code.take().unwrap())
                    .expect("machine-readable citation JSON");
                collect(&value, &mut out);
            }
            _ => {}
        }
    }
    out
}
fn assert_navigation(
    f: &Fixture,
    page: &VaultRelativePath,
    markdown: &str,
    targets: &[VaultRelativePath],
) {
    let base = url::Url::from_file_path(f.root.join(page.as_str())).unwrap();
    let root = fs::canonicalize(&f.root).unwrap();
    let mut actual = Vec::new();
    for event in Parser::new(block(markdown)) {
        if let Event::Start(Tag::Link { dest_url, .. }) = event {
            assert!(
                !dest_url.contains('#'),
                "byte spans are not Markdown fragments"
            );
            assert!(
                !dest_url.starts_with('/'),
                "provenance destinations are relative"
            );
            let resolved = base.join(&dest_url).unwrap().to_file_path().unwrap();
            let resolved =
                fs::canonicalize(resolved).expect("saved Page href resolves to a real file");
            assert!(resolved.starts_with(&root), "href stays in the vault");
            actual.push(resolved);
        }
    }
    let expected: Vec<_> = targets
        .iter()
        .map(|p| fs::canonicalize(f.root.join(p.as_str())).unwrap())
        .collect();
    assert_eq!(
        actual.len(),
        expected.len(),
        "exactly Source, Revision and captured-content navigation per distinct ref"
    );
    for p in expected {
        assert!(
            actual.contains(&p),
            "missing authenticated target {}",
            p.display()
        );
    }
}

#[test]
fn cited_page_rename_keeps_original_targets_and_exact_refs_at_new_depth() {
    for normalized in [false, true] {
        let f = Fixture::new(normalized);
        let (_, citation, targets) = f.add("# Route\nOriginal cited fact café.\n");
        let from = path("pages/cited.md");
        let to = path("pages/deep/renamed % (café)#.md");
        let initial = f
            .app
            .page_initialize_with_source_refs(
                Some(from.clone()),
                None,
                "Cited relocation".into(),
                "# Answer\nAuthor wording remains unchanged.\n".into(),
                refs(&[citation.clone()]),
            )
            .unwrap();
        let original = f.bytes(&from);
        let original_text = std::str::from_utf8(&original).unwrap();
        let author = outside(original_text);
        f.app
            .page_rename(
                initial.allocated_ids[from.as_str()].clone(),
                to.clone(),
                Blake3Hash::digest(&original),
            )
            .unwrap();
        assert!(!f.root.join(from.as_str()).exists());
        let moved = f.bytes(&to);
        let moved_text = std::str::from_utf8(&moved).unwrap();
        assert_eq!(outside(moved_text), author);
        assert_eq!(decoded_citations(moved_text), vec![citation]);
        assert_navigation(&f, &to, moved_text, &targets);
    }
}

#[test]
fn citation_markers_inside_author_code_or_html_never_grant_ownership() {
    for normalized in [false, true] {
        let f = Fixture::new(normalized);
        let (_, citation, _) = f.add("Marker ownership control.\n");
        for body in [
            format!("# Example\n```text\n{BEGIN}\nAuthor's literal example.\n{END}\n```\n"),
            format!("# Example\n<pre>\n{BEGIN}\nAuthor's literal example.\n{END}\n</pre>\n"),
            format!("# Example\n<div>\n{BEGIN}\nAuthor's literal example.\n{END}\n</div>\n"),
            "# Example\n```text\nAn unclosed authored example.\n".into(),
            "# Example\n<!-- An unclosed authored HTML comment.\n".into(),
        ] {
            let target = path("pages/literal-markers.md");
            assert!(
                f.app
                    .page_initialize_with_source_refs(
                        Some(target.clone()),
                        None,
                        "Literal markers".into(),
                        body,
                        refs(&[citation.clone()]),
                    )
                    .is_err()
            );
            assert!(!f.root.join(target.as_str()).exists());
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
struct Entry {
    modified: SystemTime,
    directory: bool,
    bytes: Vec<u8>,
}
fn tree(root: &Path) -> BTreeMap<PathBuf, Entry> {
    fn visit(root: &Path, p: &Path, out: &mut BTreeMap<PathBuf, Entry>) {
        let m = fs::symlink_metadata(p).unwrap();
        assert!(!m.file_type().is_symlink());
        out.insert(
            p.strip_prefix(root).unwrap().to_owned(),
            Entry {
                modified: m.modified().unwrap(),
                directory: m.is_dir(),
                bytes: if m.is_file() {
                    fs::read(p).unwrap()
                } else {
                    vec![]
                },
            },
        );
        if m.is_dir() {
            for child in fs::read_dir(p).unwrap() {
                visit(root, &child.unwrap().path(), out);
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out);
    out
}

#[test]
fn typed_citations_saved_allocated_and_nested_pages_resolve_authenticated_targets() {
    for normalized in [false, true] {
        let f = Fixture::new(normalized);
        let (_, citation, targets) = f.add("Café 東京 🦀: the relay uses Cedar.\n");
        for destination in [
            None,
            Some(path("handbook/東京 #100% (draft)/Guide café.md")),
        ] {
            let body = "# Supported statement\r\nCafé relay uses Cedar.\r\n";
            let saved = f
                .app
                .page_initialize_with_source_refs(
                    destination,
                    None,
                    "Café guide".into(),
                    body.into(),
                    refs(&[citation.clone(), citation.clone()]),
                )
                .unwrap();
            let page = path(saved.allocated_ids.keys().next().unwrap());
            let markdown = String::from_utf8(f.bytes(&page)).unwrap();
            assert!(
                markdown.contains(body),
                "author body bytes survive initialization"
            );
            let record = crate::records::parse_note(markdown.as_bytes())
                .canonical
                .unwrap();
            assert_eq!(record.kind(), RecordKind::Page);
            assert_eq!(record.string("wiki_status"), Some("draft"));
            assert_eq!(
                decoded_citations(&markdown),
                vec![citation.clone()],
                "exact duplicates keep only their first occurrence"
            );
            assert_navigation(&f, &page, &markdown, &targets);
        }
    }
}

#[test]
fn typed_citations_guarded_replacement_preserves_author_bytes_and_noop_guards() {
    for normalized in [false, true] {
        let f = Fixture::new(normalized);
        let (_, citation, _) = f.add("Relay uses Cedar.\n");
        let p = path("pages/guide.md");
        f.app
            .page_initialize_with_source_refs(
                Some(p.clone()),
                None,
                "Guide".into(),
                "Original author text.\r\n".into(),
                refs(&[citation.clone()]),
            )
            .unwrap();
        let initial = f.bytes(&p);
        let mut authored = initial.clone();
        authored.extend_from_slice(b"\r\nAuthor note: preserve exactly.\r\n");
        f.app
            .page_put(
                p.clone(),
                authored.clone(),
                Some(Blake3Hash::digest(&initial)),
            )
            .unwrap();
        let stale = f.app.page_put_with_source_refs(
            p.clone(),
            authored.clone(),
            Some(Blake3Hash::digest(&initial)),
            refs(&[citation.clone()]),
        );
        assert_eq!(stale.unwrap_err().code, ErrorCode::ContentConflict);
        assert_eq!(f.bytes(&p), authored);
        let before = outside(std::str::from_utf8(&authored).unwrap());
        let no_op = f
            .app
            .page_put_with_source_refs(
                p.clone(),
                authored.clone(),
                Some(Blake3Hash::digest(&authored)),
                refs(&[citation.clone()]),
            )
            .unwrap();
        assert!(no_op.reused && no_op.change.is_none());
        assert!(
            !no_op.plan.read_preconditions.is_empty(),
            "no-op retains observed Source guards"
        );
        assert_eq!(f.bytes(&p), authored);
        let removed = f
            .app
            .page_put_with_source_refs(
                p.clone(),
                authored.clone(),
                Some(Blake3Hash::digest(&authored)),
                refs(&[]),
            )
            .unwrap();
        assert!(!removed.reused);
        let without = String::from_utf8(f.bytes(&p)).unwrap();
        assert!(!without.contains(BEGIN) && !without.contains(END));
        assert_eq!(
            without,
            format!("{}{}", before.0, before.1),
            "empty refs remove only the owned block"
        );
    }
}

#[test]
fn typed_citations_html_capture_uses_declared_content_and_exact_unicode_span() {
    for normalized in [false, true] {
        let f = Fixture::new(normalized);
        let content = "# Café 東京\n\nRelay uses Cedar 🦀.\n";
        let captured = f
            .app
            .source_add(CaptureRequest {
                title: "HTML facts".into(),
                origin_kind: SourceOrigin::LocalFile,
                origin: "fixture.html".into(),
                original: "<h1>Café 東京</h1><p>Relay uses Cedar 🦀.</p>"
                    .as_bytes()
                    .to_vec(),
                extraction: ExtractionInput::Supplied {
                    extractor: "fixture-html-text-v1".into(),
                    fingerprint: Blake3Hash::digest(b"fixture-html-text-v1"),
                    content: content.as_bytes().to_vec(),
                },
                media_type: Some("text/html".into()),
            })
            .unwrap();
        let (whole, targets) = f.reference(
            &captured.allocated_ids["source"],
            &captured.allocated_ids["revision"],
        );
        let CitationRef::Source(mut exact) = whole else {
            unreachable!()
        };
        let quote = "Relay uses Cedar 🦀.";
        let start = content.find(quote).unwrap();
        exact.span = ByteSpan::new(start as u64, (start + quote.len()) as u64).unwrap();
        exact.quote_hash = Blake3Hash::digest(quote.as_bytes());
        let citation = CitationRef::Source(exact);
        let p = path("pages/html/café (captured).md");
        f.app
            .page_initialize_with_source_refs(
                Some(p.clone()),
                None,
                "Captured HTML".into(),
                "Relay uses Cedar.\n".into(),
                refs(&[citation.clone()]),
            )
            .unwrap();
        let markdown = String::from_utf8(f.bytes(&p)).unwrap();
        assert_eq!(decoded_citations(&markdown), vec![citation]);
        assert_navigation(&f, &p, &markdown, &targets);
        assert_eq!(f.bytes(&targets[2]), content.as_bytes());
    }
}

#[test]
fn typed_citations_refresh_and_withdraw_keep_immutable_links_and_exact_refs() {
    for normalized in [false, true] {
        let f = Fixture::new(normalized);
        let (source, old, old_targets) = f.add("Old café policy: Cedar.\n");
        let immutable: Vec<_> = old_targets[1..]
            .iter()
            .map(|p| (p.clone(), f.bytes(p)))
            .collect();
        let p = path("pages/lifecycle.md");
        f.app
            .page_initialize_with_source_refs(
                Some(p.clone()),
                None,
                "Lifecycle".into(),
                "Old policy.\n".into(),
                refs(&[old.clone()]),
            )
            .unwrap();
        let first = f.bytes(&p);
        let refreshed = f
            .app
            .source_refresh(source.clone(), capture("New café policy: Birch.\n"))
            .unwrap();
        let (new, new_targets) = f.reference(&source, &refreshed.allocated_ids["revision"]);
        assert_ne!(new, old);
        f.app
            .page_put_with_source_refs(
                p.clone(),
                first.clone(),
                Some(Blake3Hash::digest(&first)),
                refs(&[old.clone(), new.clone()]),
            )
            .unwrap();
        let updated = f.bytes(&p);
        let markdown = std::str::from_utf8(&updated).unwrap();
        assert_eq!(
            outside(markdown),
            outside(std::str::from_utf8(&first).unwrap())
        );
        assert_eq!(decoded_citations(markdown), vec![old.clone(), new.clone()]);
        assert!(block(markdown).to_lowercase().contains("historical"));
        assert!(block(markdown).to_lowercase().contains("current"));
        let targets = [old_targets, new_targets].concat();
        assert_navigation(&f, &p, markdown, &targets);
        f.app.source_withdraw(source, "Fixture withdrawal").unwrap();
        f.app
            .page_put_with_source_refs(
                p.clone(),
                updated.clone(),
                Some(Blake3Hash::digest(&updated)),
                refs(&[old.clone(), new.clone()]),
            )
            .unwrap();
        let withdrawn = String::from_utf8(f.bytes(&p)).unwrap();
        assert!(block(&withdrawn).to_lowercase().contains("withdrawn"));
        assert_eq!(decoded_citations(&withdrawn), vec![old, new]);
        assert_navigation(&f, &p, &withdrawn, &targets);
        for (target, bytes) in immutable {
            assert_eq!(f.bytes(&target), bytes);
        }
    }
}

#[test]
fn typed_citations_bad_quotes_utf8_and_ownership_refuse_before_page_mutation() {
    for normalized in [false, true] {
        let f = Fixture::new(normalized);
        let (_, valid, _) = f.add("Café 東京 facts.\n");
        let (other_source, _, _) = f.add("Other owner.\n");
        let p = path("pages/refusal.md");
        f.app
            .page_initialize(
                Some(p.clone()),
                None,
                "Refusal".into(),
                "Keep author bytes.\n".into(),
            )
            .unwrap();
        let original = f.bytes(&p);
        let CitationRef::Source(reference) = valid else {
            unreachable!()
        };
        let mut bad_quote = reference.clone();
        bad_quote.quote_hash = Blake3Hash::digest(b"different quote");
        let mut split = reference.clone();
        split.span = ByteSpan::new(4, 5).unwrap();
        let mut wrong_owner = reference;
        wrong_owner.source_id = other_source;
        for invalid in [bad_quote, split, wrong_owner] {
            assert!(
                f.app
                    .page_put_with_source_refs(
                        p.clone(),
                        original.clone(),
                        Some(Blake3Hash::digest(&original)),
                        refs(&[CitationRef::Source(invalid)])
                    )
                    .is_err()
            );
            assert_eq!(f.bytes(&p), original);
        }
    }
}

#[test]
fn typed_citations_duplicate_and_malformed_owned_markers_refuse_without_repair() {
    for normalized in [false, true] {
        let f = Fixture::new(normalized);
        let (_, citation, _) = f.add("Local exact facts.\n");
        let p = path("pages/markers.md");
        f.app
            .page_initialize(
                Some(p.clone()),
                None,
                "Markers".into(),
                "Original.\n".into(),
            )
            .unwrap();
        let original = f.bytes(&p);
        for markers in [
            format!("{BEGIN}\nmissing end"),
            format!("{END}\n{BEGIN}"),
            format!("{BEGIN}\n{END}\n{BEGIN}\n{END}"),
            "<!-- lwiki:source-citations:v1 begin-->".into(),
        ] {
            let mut proposal = original.clone();
            proposal.extend_from_slice(markers.as_bytes());
            assert!(
                f.app
                    .page_put_with_source_refs(
                        p.clone(),
                        proposal,
                        Some(Blake3Hash::digest(&original)),
                        refs(&[citation.clone()])
                    )
                    .is_err()
            );
            assert_eq!(f.bytes(&p), original);
        }
    }
}

#[test]
fn typed_citations_rendered_markdown_counts_toward_existing_page_byte_ceiling() {
    for normalized in [false, true] {
        let f = Fixture::new(normalized);
        let (_, citation, _) = f.add("Small exact source.\n");
        let p = path("pages/output-bound.md");
        f.app
            .page_initialize(Some(p.clone()), None, "Bound".into(), "Original.\n".into())
            .unwrap();
        let original = f.bytes(&p);
        // The author proposal itself fits; only the generated citation makes
        // the final file too large. No large canonical fixture is written.
        let mut at_limit = original.clone();
        at_limit.resize(super::MAX_INPUT_BYTES, b'x');
        let failure = f
            .app
            .page_put_with_source_refs(
                p.clone(),
                at_limit,
                Some(Blake3Hash::digest(&original)),
                refs(&[citation]),
            )
            .unwrap_err();
        assert_eq!(failure.code, ErrorCode::BudgetExceeded);
        assert_eq!(f.bytes(&p), original);
    }
}

#[test]
fn typed_citations_request_schema_is_strict_and_bounded() {
    let reference = json!({"source_id":"source_fixture", "source_revision":"revision_fixture", "span":{"start":0,"end":1}, "quote_hash":Blake3Hash::digest(b"a")});
    let citation = json!({"kind":"source", "reference":reference});
    let valid = json!({"schema_version":"1", "citations":[citation.clone()]});
    assert_eq!(
        PageSourceRefs::from_json_slice(&serde_json::to_vec(&valid).unwrap())
            .unwrap()
            .len(),
        1
    );
    for invalid in [
        json!({"schema_version":"2","citations":[]}),
        json!({"schema_version":1,"citations":[]}),
        json!({"schema_version":"1","citations":[],"path":"sources/fake.md"}),
        json!({"schema_version":"1","citations":[{"kind":"assertion","reference":{"evidence_id":"evidence_fixture","assertion_id":"assertion_fixture","source_id":"source_fixture","source_revision":"revision_fixture","span":{"start":0,"end":1},"quote_hash":Blake3Hash::digest(b"a")}}]}),
        json!({"schema_version":"1","citations":[{"kind":"source","reference":reference.clone(),"status":"current"}]}),
        json!({"schema_version":"1","citations":[{"kind":"source","reference":{"source_id":"source_fixture","source_revision":"revision_fixture","span":{"start":0,"end":1},"quote_hash":Blake3Hash::digest(b"a"),"path":"fake.md"}}]}),
    ] {
        assert!(
            PageSourceRefs::from_json_slice(&serde_json::to_vec(&invalid).unwrap()).is_err(),
            "accepted {invalid}"
        );
    }
    for invalid in [
        r#"{"schema_version":"1","schema_version":"1","citations":[]}"#.to_owned(),
        format!(
            r#"{{"schema_version":"1","citations":[{{"kind":"source","kind":"source","reference":{reference}}}]}}"#
        ),
        format!(
            r#"{{"schema_version":"1","citations":[{{"kind":"source","reference":{{"source_id":"source_fixture","source_id":"source_fixture","source_revision":"revision_fixture","span":{{"start":0,"end":1}},"quote_hash":"{}"}}}}]}}"#,
            Blake3Hash::digest(b"a")
        ),
    ] {
        assert!(PageSourceRefs::from_json_slice(invalid.as_bytes()).is_err());
    }
    let many: Vec<_> = (0..17)
        .map(|n| {
            let mut c = citation.clone();
            c["reference"]["span"] = json!({"start":n,"end":n+1});
            c
        })
        .collect();
    assert!(
        PageSourceRefs::from_json_slice(
            &serde_json::to_vec(&json!({"schema_version":"1","citations":many})).unwrap()
        )
        .is_err()
    );
    let mut oversized = serde_json::to_vec(&valid).unwrap();
    oversized.resize(65537, b' ');
    assert!(PageSourceRefs::from_json_slice(&oversized).is_err());
}

#[test]
fn typed_citations_dry_run_skips_source_verification_and_preserves_entire_fixture() {
    for normalized in [false, true] {
        let f = Fixture::new(normalized);
        let (_, valid, _) = f.add("Preview source.\n");
        let CitationRef::Source(mut invalid) = valid else {
            unreachable!()
        };
        invalid.source_id = RecordId::new("source_not_present").unwrap();
        invalid.quote_hash = Blake3Hash::digest(b"never authenticated");
        let p = path("pages/preview.md");
        f.app
            .page_initialize(
                Some(p.clone()),
                None,
                "Preview".into(),
                "Keep preview bytes.\n".into(),
            )
            .unwrap();
        let original = f.bytes(&p);
        let preview = f.with_options(OperationOptions {
            dry_run: true,
            ..f.app.options().clone()
        });
        let before = tree(&f.root);
        let outcome = preview
            .page_put_with_source_refs(
                p.clone(),
                original.clone(),
                Some(Blake3Hash::digest(&original)),
                refs(&[CitationRef::Source(invalid.clone())]),
            )
            .unwrap();
        assert!(outcome.change.is_none() && outcome.status.is_none());
        assert!(outcome.plan.read_preconditions.is_empty());
        let initialized = preview
            .page_initialize_with_source_refs(
                None,
                None,
                "Unresolved preview".into(),
                "No generated links yet.\n".into(),
                refs(&[CitationRef::Source(invalid)]),
            )
            .unwrap();
        assert!(initialized.change.is_none() && initialized.status.is_none());
        assert_eq!(
            tree(&f.root),
            before,
            "preview changes no canonical/cache/coordination bytes or mtimes"
        );
        assert_eq!(f.bytes(&p), original);
    }
}

#[test]
fn typed_citations_staged_page_guards_source_changes_and_publication_exclusion() {
    for normalized in [false, true] {
        let f = Fixture::new(normalized);
        let (source, citation, targets) = f.add("Before refresh: Cedar.\n");
        let p = path("pages/staged.md");
        f.app
            .page_initialize(
                Some(p.clone()),
                None,
                "Staged".into(),
                "Original authored text.\n".into(),
            )
            .unwrap();
        let original = f.bytes(&p);
        let staged_app = f.with_options(OperationOptions {
            stage_only: true,
            ..f.app.options().clone()
        });
        let staged = staged_app
            .page_put_with_source_refs(
                p.clone(),
                original.clone(),
                Some(Blake3Hash::digest(&original)),
                refs(&[citation]),
            )
            .unwrap();
        assert!(
            staged
                .plan
                .read_preconditions
                .iter()
                .any(|dependency| dependency.path == targets[0]),
            "mutable Source observation participates in actual commit guards"
        );
        assert_eq!(f.bytes(&p), original);
        if normalized {
            // A prepared normalized publication excludes another ordinary
            // writer. An external author can still change the guarded Source.
            let refusal = f
                .app
                .source_refresh(source, capture("After refresh: Birch.\n"))
                .unwrap_err();
            assert_eq!(refusal.code, ErrorCode::RecoveryRequired);
            let mut external = f.bytes(&targets[0]);
            external.extend_from_slice(b"\nExternal Source owner edit while prepared.\n");
            fs::write(f.root.join(targets[0].as_str()), external).unwrap();
        } else {
            f.app
                .source_refresh(source, capture("After refresh: Birch.\n"))
                .unwrap();
        }
        let refusal = f
            .app
            .changes_apply(staged.change.unwrap().change_id)
            .unwrap_err();
        assert!(
            matches!(
                refusal.code,
                ErrorCode::ContentConflict | ErrorCode::FreshnessConflict
            ),
            "changed Source must fail the actual guarded apply: {refusal:?}"
        );
        assert_eq!(f.bytes(&p), original);
    }
}

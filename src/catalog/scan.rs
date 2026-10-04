//! Deterministic canonical projection. This module performs no extraction or model calls.
use super::types::*;
use crate::{
    changes::{ReadDependency, ScanDocument, ValidationInput},
    domain::{
        Blake3Hash, Eligibility, ErrorCode, RecordId, RecordKind, Result, VaultRelativePath,
        WikiError,
    },
    records::{
        LinkResolution, ParsedNote, RegistryEntry, extract_links, links::IndexedRegistry,
        parse_note,
    },
    sources::{SourceView, identity::readable_ids, revision::canonical_path},
    vault::{ExpectedState, VaultFs},
};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use std::collections::{BTreeMap, BTreeSet};

/// Cache identity binds every semantic adapter, not merely frontmatter parsing.
pub fn parser_fingerprint() -> Blake3Hash {
    Blake3Hash::digest(format!(
        "{};catalog-v1;canonical-membership-bytewise-v1;source-original-content-span-quote-fence-structural-v2;typed-id-companion-v1;decisions-explicit-conflict-cycle-v1;mention-complete-artifact-membership-explicit-scope-v1;entity-explicit-exhaustive-remap-receipt-supersession-v1;receipt-relevant-proof-budget-v2;eligibility-full-note-transitive-v1;packet-source-lifecycle-v2;derived-source-lifecycle-v1;opposing-accepted-assertions-v1;evidence-navigation-diagnostics-v1;bookkeeping-kind-v2;markdown-lexical-events-v1;graph-readable-directed-endpoints-v1;unicode61 remove_diacritics 2;no-stemming",
        crate::records::parser_fingerprint()
    ))
}

pub fn scan(fs: &VaultFs, vault_id: &RecordId) -> Result<CatalogProjection> {
    project(fs, &scan_input(fs, vault_id)?)
}

pub(crate) fn scan_input(fs: &VaultFs, vault_id: &RecordId) -> Result<ValidationInput> {
    let mut documents = Vec::new();
    for path in fs.root().scan_markdown()? {
        let before = fs.read_before(&path)?.ok_or_else(|| {
            WikiError::new(
                ErrorCode::ContentConflict,
                "canonical path disappeared during scan",
            )
        })?;
        documents.push(ScanDocument {
            path,
            bytes: before.bytes,
            hash: before.hash,
        });
    }
    Ok(ValidationInput {
        vault_id: vault_id.clone(),
        documents,
        overlay: vec![],
    })
}

pub(crate) fn input_notes(
    input: &ValidationInput,
) -> Result<BTreeMap<VaultRelativePath, ParsedNote>> {
    let mut notes = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for document in &input.documents {
        if !seen.insert(document.path.clone()) {
            return Err(WikiError::invalid("duplicate canonical scan path"));
        }
        if Blake3Hash::digest(&document.bytes) != document.hash {
            return Err(WikiError::new(
                ErrorCode::ContentConflict,
                "canonical scan hash mismatch",
            ));
        }
        if canonical_path(&document.path) {
            notes.insert(document.path.clone(), parse_note(&document.bytes));
        }
    }
    seen.clear();
    for target in &input.overlay {
        if !seen.insert(target.path.clone()) {
            return Err(WikiError::invalid("duplicate proposed path"));
        }
        if canonical_path(&target.path) {
            match &target.bytes {
                Some(bytes) => {
                    notes.insert(target.path.clone(), parse_note(bytes));
                }
                None => {
                    notes.remove(&target.path);
                }
            }
        }
    }
    Ok(notes)
}

/// A safely delimited scalar ID still reserves identity when later metadata is invalid.
pub(crate) fn readable_id(note: &ParsedNote) -> Option<RecordId> {
    let ids = readable_ids(note);
    if ids.len() == 1 {
        ids.into_iter().next()
    } else {
        None
    }
}

pub(crate) use crate::sources::identity::isolated_fields;

pub(crate) fn diagnostic(
    path: &VaultRelativePath,
    id: Option<&RecordId>,
    code: ErrorCode,
    details: serde_json::Value,
) -> CatalogDiagnostic {
    CatalogDiagnostic {
        path: path.clone(),
        record_id: id.cloned(),
        code,
        details,
    }
}

pub fn project(fs: &VaultFs, input: &ValidationInput) -> Result<CatalogProjection> {
    project_catalog(fs, input, false)
}

/// Reconstruct derived authority using only a complete, caller-metered input.
pub(crate) fn project_closed(fs: &VaultFs, input: &ValidationInput) -> Result<CatalogProjection> {
    project_catalog(fs, input, true)
}

/// Validation retains complete graph authority and observed dependencies without
/// constructing retrieval rows. Existing input/view buffers still retain bytes;
/// this skips the additional corpus of raw and normalized retrieval text.
pub(crate) fn project_validation(
    fs: &VaultFs,
    input: &ValidationInput,
) -> Result<ValidationProjection> {
    project_input(fs, input, false, None, None)
}

pub(crate) fn project_validation_closed(
    fs: &VaultFs,
    input: &ValidationInput,
) -> Result<ValidationProjection> {
    project_input(fs, input, true, None, None)
}

#[derive(Default)]
struct CollectingRetrievalSink {
    documents: Vec<DocumentRow>,
    links: Vec<LinkRow>,
    graph: Vec<GraphRow>,
}

impl RetrievalSink for CollectingRetrievalSink {
    fn identity_claim(&mut self, _row: IdentityClaimRow) -> Result<()> {
        Ok(())
    }
    fn document(&mut self, row: DocumentRow) -> Result<()> {
        self.documents.push(row);
        Ok(())
    }
    fn graph(&mut self, row: GraphRow) -> Result<()> {
        self.graph.push(row);
        Ok(())
    }
    fn link(&mut self, row: LinkRow) -> Result<()> {
        self.links.push(row);
        Ok(())
    }
}
impl CollectingRetrievalSink {
    fn sort(&mut self) {
        self.documents
            .sort_by(|a, b| a.path.as_str().as_bytes().cmp(b.path.as_str().as_bytes()));
        self.links
            .sort_by(|a, b| (&a.from_path, a.byte_start).cmp(&(&b.from_path, b.byte_start)));
    }
}

/// Emit owned retrieval rows while retaining complete validation authority.
/// Rows are provisional until this returns successfully and the caller seals
/// its publication. Emission order is deterministic, but not globally sorted.
pub(crate) fn project_with_sink(
    fs: &VaultFs,
    input: &ValidationInput,
    closed: bool,
    sink: &mut dyn RetrievalSink,
) -> Result<ValidationProjection> {
    project_input(fs, input, closed, Some(sink), None)
}

/// A complete normalized build avoids constructing legacy transitive proof
/// copies. Its separate result type must not authorize legacy proof consumers.
pub(crate) fn project_normalized_with_sink(
    fs: &VaultFs,
    input: &ValidationInput,
    closed: bool,
    sink: &mut dyn RetrievalSink,
) -> Result<NormalizedValidationProjection> {
    let mut facts = super::eligibility_facts::NormalizedEligibilityFacts::new();
    let validation = project_input(fs, input, closed, Some(sink), Some(&mut facts))?;
    Ok(NormalizedValidationProjection { validation, facts })
}

fn project_catalog(
    fs: &VaultFs,
    input: &ValidationInput,
    closed: bool,
) -> Result<CatalogProjection> {
    let mut retrieval = CollectingRetrievalSink::default();
    let validation = project_with_sink(fs, input, closed, &mut retrieval)?;
    retrieval.sort();
    Ok(CatalogProjection {
        vault_id: validation.vault_id,
        parser_fingerprint: validation.parser_fingerprint,
        control_manifest: validation.control_manifest,
        documents: retrieval.documents,
        records: validation.records,
        graph: retrieval.graph,
        links: retrieval.links,
        diagnostics: validation.diagnostics,
        dependencies: validation.dependencies,
    })
}

fn project_input(
    fs: &VaultFs,
    input: &ValidationInput,
    closed: bool,
    mut retrieval: Option<&mut dyn RetrievalSink>,
    mut normalized: Option<&mut super::eligibility_facts::NormalizedEligibilityFacts>,
) -> Result<ValidationProjection> {
    let notes = input_notes(input)?;
    let mut memberships: BTreeMap<RecordId, Vec<VaultRelativePath>> = BTreeMap::new();
    for (path, note) in &notes {
        for id in readable_ids(note) {
            if let Some(sink) = retrieval.as_deref_mut() {
                sink.identity_claim(IdentityClaimRow {
                    id: id.clone(),
                    path: path.clone(),
                    hash: note.source_hash.clone(),
                    kind: note.canonical.as_ref().map(|record| record.kind()),
                })?;
            }
            memberships.entry(id).or_default().push(path.clone());
        }
    }
    let mut diagnostics = Vec::new();
    let mut records = BTreeMap::new();
    for (path, note) in &notes {
        let id = readable_id(note);
        for claimed in readable_ids(note) {
            if let Some(paths) = memberships.get(&claimed).filter(|paths| paths.len() > 1) {
                diagnostics.push(diagnostic(
                    path,
                    Some(&claimed),
                    ErrorCode::ReferenceAmbiguous,
                    serde_json::json!({"reason":"duplicate_id", "paths":paths}),
                ));
            }
        }
        for error in &note.diagnostics {
            diagnostics.push(diagnostic(
                path,
                id.as_ref(),
                error.code,
                serde_json::json!({"message":error.message,"details":error.details}),
            ));
        }
        if let Some(record) = &note.canonical {
            if memberships
                .get(record.id())
                .is_some_and(|paths| paths.len() != 1)
            {
                continue;
            }
            records.insert(
                record.id().clone(),
                RecordRow {
                    record: record.clone(),
                    path: path.clone(),
                    hash: note.source_hash.clone(),
                    authored_status: record.string("wiki_status").map(str::to_owned),
                    eligibility: Eligibility::Current,
                    reasons: vec![],
                    identity_eligibility: None,
                    description_eligibility: None,
                    disputed: false,
                    dependencies: vec![ReadDependency {
                        path: path.clone(),
                        expected: ExpectedState::Hash(note.source_hash.clone()),
                    }],
                },
            );
        }
    }
    let source_view = if closed {
        SourceView::from_closed_input(fs, input)?
    } else {
        SourceView::from_input(fs, input)?
    };
    if let Some(facts) = normalized.as_deref_mut() {
        *facts = super::eligibility::compute_normalized(
            &source_view,
            &notes,
            &mut records,
            &mut diagnostics,
        )?;
    } else {
        super::eligibility::compute(&source_view, &notes, &mut records, &mut diagnostics)?;
    }
    let mut dependencies: BTreeMap<VaultRelativePath, ExpectedState> = notes
        .iter()
        .map(|(p, n)| (p.clone(), ExpectedState::Hash(n.source_hash.clone())))
        .collect();
    if let Some(facts) = normalized.as_deref() {
        for (path, expected) in &facts.observed {
            if dependencies.get(path).is_some_and(|old| old != expected) {
                return Err(WikiError::new(
                    ErrorCode::ContentConflict,
                    "normalized observation conflicts with canonical input",
                ));
            }
            dependencies.insert(path.clone(), expected.clone());
        }
    }
    if let Some(retrieval) = retrieval.as_deref_mut() {
        if normalized.is_some() {
            for row in records.values() {
                let entry = RegistryEntry {
                    id: row.record.id().clone(),
                    kind: row.record.kind(),
                    path: row.path.clone(),
                    aliases: list(&row.record, "aliases"),
                };
                retrieval.registry_keys(&entry, &super::link_facts::registry_keys(&entry)?)?;
            }
        }
        let registry = IndexedRegistry::new(
            records
                .values()
                .map(|row| RegistryEntry {
                    id: row.record.id().clone(),
                    kind: row.record.kind(),
                    path: row.path.clone(),
                    aliases: list(&row.record, "aliases"),
                })
                .collect(),
        );
        for (path, note) in &notes {
            let row = note
                .canonical
                .as_ref()
                .and_then(|record| records.get(record.id()))
                .filter(|row| &row.path == path);
            retrieval.document(super::row_projection::canonical_document(path, note, row))?;
            let body = std::str::from_utf8(note.body()).unwrap_or_default();
            let body_offset = note.raw.len() - note.body().len();
            for link in extract_links(body) {
                let resolution = registry.resolve_untyped(&link.destination);
                let (target_id, target_path) = match &resolution {
                    LinkResolution::Resolved { id, path, .. } => {
                        (Some(id.clone()), Some(path.clone()))
                    }
                    _ => (None, None),
                };
                retrieval.link(LinkRow {
                    from_path: path.clone(),
                    byte_start: (body_offset + link.range.start) as u64,
                    target_id,
                    target_path,
                    resolution: if normalized.is_some() {
                        format!(
                            "{:?}",
                            super::navigation_resolution::NavigationResolution::from(&resolution)
                        )
                    } else {
                        format!("{resolution:?}")
                    },
                })?;
                if normalized.is_some() {
                    retrieval.link_fact(super::link_facts::untyped_fact(
                        path,
                        (body_offset + link.range.start) as u64,
                        &link.destination,
                        &resolution,
                    )?)?;
                }
            }
            if let Some(row) = row {
                for (field, kind, companion) in super::eligibility::references(&row.record) {
                    let Some(companion) = companion else {
                        continue;
                    };
                    let Some(destination) = row.record.string(companion) else {
                        continue;
                    };
                    let Some(value) = row.record.string(field) else {
                        continue;
                    };
                    // Shared eligibility::compute already checks every reference
                    // ID, including fields without companions; this cannot add a
                    // validation failure when retrieval emission is enabled.
                    let target = RecordId::new(value)?;
                    let resolution = registry.resolve_typed(&target, kind, Some(destination));
                    let (target_id, target_path) = match &resolution {
                        LinkResolution::Resolved { id, path, .. } => {
                            (Some(id.clone()), Some(path.clone()))
                        }
                        _ => (None, None),
                    };
                    let line = note.field_starts.get(companion).copied().unwrap_or(0);
                    let tail = std::str::from_utf8(&note.raw[line..]).unwrap_or_default();
                    let start = line
                        + tail
                            .lines()
                            .next()
                            .and_then(|line| line.find("[["))
                            .unwrap_or(0);
                    retrieval.link(LinkRow {
                        from_path: path.clone(),
                        byte_start: start as u64,
                        target_id,
                        target_path,
                        resolution: if normalized.is_some() {
                            format!(
                                "{:?}",
                                super::navigation_resolution::NavigationResolution::from(
                                    &resolution
                                )
                            )
                        } else {
                            format!("{resolution:?}")
                        },
                    })?;
                    if normalized.is_some() {
                        retrieval.link_fact(super::link_facts::typed_fact(
                            path,
                            start as u64,
                            destination,
                            &target,
                            kind,
                        )?)?;
                    }
                }
            }
        }
    }
    for row in records.values() {
        for dependency in &row.dependencies {
            dependencies.insert(dependency.path.clone(), dependency.expected.clone());
        }
        let record = &row.record;
        if let Some(retrieval) = retrieval.as_deref_mut()
            && matches!(record.kind(), RecordKind::Entity | RecordKind::Assertion)
        {
            let endpoints = [
                record.string("wiki_subject_id"),
                record.string("wiki_object_id"),
            ]
            .map(|id| {
                id.and_then(|id| RecordId::new(id).ok())
                    .and_then(|id| records.get(&id))
                    .map(|row| &row.record)
            });
            retrieval.graph(super::row_projection::graph_row(
                row,
                notes.get(&row.path),
                endpoints,
            )?)?;
        }
        if record.kind() == RecordKind::Revision
            && record.string("wiki_extraction_status") == Some("complete")
        {
            let source_id = RecordId::new(record.string("wiki_source_id").expect("source ID"))?;
            let mut deps = BTreeMap::new();
            if records
                .get(&source_id)
                .is_some_and(|r| r.record.kind() == RecordKind::Source)
                && let Some((parent, _)) = row.path.as_str().rsplit_once('/')
            {
                let path = VaultRelativePath::new(format!(
                    "{parent}/{}",
                    record
                        .string("wiki_content_path")
                        .expect("complete content")
                ))?;
                // Reading remains shared: even failed/invalid payloads may add
                // dependencies. Only retrieval owns decoded and normalized text.
                if let Ok(content) = source_view.read(&path, &mut deps)
                    && let Some(retrieval) = retrieval.as_deref_mut()
                    && let Ok(raw_text) = String::from_utf8(content)
                {
                    retrieval.document(super::row_projection::captured_content_document(
                        path, source_id, row, raw_text,
                    ))?;
                }
            }
            // Failed verification also contributes every byte observed before failure.
            for (path, expected) in deps {
                if normalized.is_some()
                    && dependencies.get(&path).is_some_and(|old| old != &expected)
                {
                    return Err(WikiError::new(
                        ErrorCode::ContentConflict,
                        "normalized payload observation changed during projection",
                    ));
                }
                dependencies.insert(path, expected);
            }
        }
    }
    diagnostics.sort_by(|a, b| {
        (&a.path, format!("{:?}", a.code), a.details.to_string()).cmp(&(
            &b.path,
            format!("{:?}", b.code),
            b.details.to_string(),
        ))
    });
    let control_manifest = manifest_hash(&notes);
    Ok(ValidationProjection {
        vault_id: input.vault_id.clone(),
        parser_fingerprint: parser_fingerprint(),
        control_manifest,
        records,
        diagnostics,
        dependencies: dependencies
            .into_iter()
            .map(|(path, expected)| ReadDependency { path, expected })
            .collect(),
    })
}

pub(crate) fn manifest_hash(notes: &BTreeMap<VaultRelativePath, ParsedNote>) -> Blake3Hash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lwiki-canonical-control-v1\0");
    for (path, note) in notes {
        hasher.update(&(path.as_str().len() as u64).to_le_bytes());
        hasher.update(path.as_str().as_bytes());
        hasher.update(&(note.raw.len() as u64).to_le_bytes());
        hasher.update(&note.raw);
    }
    Blake3Hash::new(format!("blake3:{}", hasher.finalize().to_hex()))
        .expect("BLAKE3 emits 64 lowercase hexadecimal digits")
}

pub(crate) fn list(record: &crate::domain::CanonicalRecord, key: &str) -> Vec<String> {
    record
        .field(key)
        .and_then(serde_json::Value::as_array)
        .map(|v| {
            v.iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
thread_local! {
    static NORMALIZATION_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(crate) fn normalized_markdown(markdown: &str) -> (String, String) {
    #[cfg(test)]
    NORMALIZATION_CALLS.with(|calls| calls.set(calls.get() + 1));
    let mut text = String::new();
    let mut headings = Vec::new();
    let mut heading = None;
    for event in Parser::new(markdown) {
        match event {
            Event::Start(Tag::Heading { .. }) => heading = Some(String::new()),
            Event::End(TagEnd::Heading(_)) => {
                if let Some(value) = heading.take() {
                    headings.push(value);
                }
                text.push('\n');
            }
            Event::Text(value) | Event::Code(value) => {
                text.push_str(&value);
                if let Some(heading) = &mut heading {
                    heading.push_str(&value);
                }
            }
            Event::SoftBreak | Event::HardBreak => text.push('\n'),
            Event::End(TagEnd::Paragraph | TagEnd::CodeBlock | TagEnd::Item) => text.push('\n'),
            _ => {}
        }
    }
    (headings.join("\n"), text)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        changes::{GraphValidator, ProposedTarget},
        domain::CanonicalRecord,
        sources::evidence::exact_quote_body,
        vault::VaultRoot,
    };
    use serde_json::{Value, json};
    use std::{fs, path::Path};

    const CONTENT: &[u8] = b"# Captured source\nA uses B.\n";
    const CONTENT_PATH: &str = "sources/source_a/revisions/revision_a/content.md";
    const ORIGINAL_PATH: &str = "sources/source_a/revisions/revision_a/original.bin";

    fn id(value: &str) -> RecordId {
        RecordId::new(value).unwrap()
    }
    fn path(value: &str) -> VaultRelativePath {
        VaultRelativePath::new(value).unwrap()
    }
    fn envelope(kind: &str, name: &str, extra: Value, body: &[u8]) -> Vec<u8> {
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
                .map(|(key, value)| (key.clone(), value.clone())),
        );
        CanonicalRecord::new(fields.clone()).unwrap();
        let mut bytes = b"---\n".to_vec();
        for (key, value) in fields {
            bytes.extend_from_slice(format!("{key}: {value}\n").as_bytes());
        }
        bytes.extend_from_slice(b"---\n");
        bytes.extend_from_slice(body);
        bytes
    }
    fn write(root: &Path, name: &str, bytes: &[u8]) {
        let target = root.join(name);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, bytes).unwrap();
    }
    fn fixture() -> (tempfile::TempDir, VaultFs, ValidationInput) {
        let temp = tempfile::tempdir().unwrap();
        write(
            temp.path(),
            "WIKI.md",
            &envelope("vault", "vault_projection", json!({}), b"Wiki\n"),
        );
        // Reuse the tracked P08 identity corpus, including unsupported descriptions.
        for (name, bytes) in [
            (
                "a",
                include_bytes!("../../tests/fixtures/p08/a.md").as_slice(),
            ),
            (
                "b",
                include_bytes!("../../tests/fixtures/p08/b.md").as_slice(),
            ),
            (
                "c",
                include_bytes!("../../tests/fixtures/p08/c.md").as_slice(),
            ),
        ] {
            write(temp.path(), &format!("entities/{name}.md"), bytes);
        }
        write(
            temp.path(),
            "assertions/use.md",
            &envelope(
                "assertion",
                "use",
                json!({
                    "wiki_status":"accepted", "wiki_subject_id":"a", "wiki_subject":"[[entities/a]]",
                    "wiki_object_id":"b", "wiki_predicate":"uses"
                }),
                b"A uses B.\n",
            ),
        );
        write(
            temp.path(),
            "page.md",
            &envelope(
                "page",
                "page",
                json!({
                    "wiki_status":"reviewed", "wiki_depends_on_ids":["use"]
                }),
                b"# Supported page\n[[entities/a]]\n",
            ),
        );
        write(
            temp.path(),
            "sources/source_a/source.md",
            &envelope(
                "source",
                "source_a",
                json!({
                    "wiki_status":"active", "wiki_origin_kind":"local-file", "wiki_origin":"fixture",
                    "wiki_current_revision":"revision_a", "wiki_revisions":["revision_a"]
                }),
                b"Source\n",
            ),
        );
        write(
            temp.path(),
            "sources/source_a/revisions/revision_a/revision.md",
            &envelope(
                "revision",
                "revision_a",
                json!({
                    "wiki_source_id":"source_a", "wiki_captured_at":"2026-09-28T00:00:00Z",
                    "wiki_original_path":"original.bin", "wiki_original_hash":Blake3Hash::digest(CONTENT),
                    "wiki_extractor":"fixture", "wiki_extractor_fingerprint":Blake3Hash::digest(b"fixture-v1"),
                    "wiki_extraction_status":"complete", "wiki_content_path":"content.md",
                    "wiki_content_hash":Blake3Hash::digest(CONTENT)
                }),
                b"Revision\n",
            ),
        );
        write(temp.path(), CONTENT_PATH, CONTENT);
        write(temp.path(), ORIGINAL_PATH, CONTENT);
        let quote = b"A uses B.";
        for (name, stance) in [("support", "supports"), ("opposition", "contradicts")] {
            write(
                temp.path(),
                &format!("evidence/{name}.md"),
                &envelope(
                    "evidence",
                    name,
                    json!({
                        "wiki_status":"active", "wiki_assertion_id":"use", "wiki_source_id":"source_a",
                        "wiki_source_revision":"revision_a", "wiki_stance":stance, "wiki_locator_kind":"utf8-bytes",
                        "wiki_span_start":18, "wiki_span_end":18 + quote.len(), "wiki_quote_hash":Blake3Hash::digest(quote)
                    }),
                    &exact_quote_body(quote, "\n", "Explanation").unwrap(),
                ),
            );
        }
        let fs = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let input = scan_input(&fs, &id("vault_projection")).unwrap();
        (temp, fs, input)
    }
    fn assert_differential(
        fs: &VaultFs,
        input: &ValidationInput,
        closed: bool,
    ) -> ValidationProjection {
        let full = if closed {
            project_closed(fs, input)
        } else {
            project(fs, input)
        }
        .unwrap();
        let validation = if closed {
            project_validation_closed(fs, input)
        } else {
            project_validation(fs, input)
        }
        .unwrap();
        assert_eq!(
            validation,
            ValidationProjection {
                vault_id: full.vault_id,
                parser_fingerprint: full.parser_fingerprint,
                control_manifest: full.control_manifest,
                records: full.records,
                diagnostics: full.diagnostics,
                dependencies: full.dependencies,
            }
        );
        validation
    }
    fn dependency<'a>(
        projection: &'a ValidationProjection,
        name: &str,
    ) -> Option<&'a ExpectedState> {
        projection
            .dependencies
            .iter()
            .find(|dependency| dependency.path.as_str() == name)
            .map(|dependency| &dependency.expected)
    }

    #[test]
    fn validation_projection_matches_existing_graph_fixture() {
        let (_temp, fs, input) = fixture();
        let validation = assert_differential(&fs, &input, false);
        assert_eq!(
            validation.records[&id("use")].eligibility,
            Eligibility::Current
        );
        assert!(validation.records[&id("use")].disputed);
        assert_eq!(
            validation.records[&id("page")].eligibility,
            Eligibility::Current
        );
        assert_eq!(
            validation.records[&id("a")].description_eligibility,
            Some(Eligibility::Unsupported)
        );
        assert_eq!(
            dependency(&validation, CONTENT_PATH),
            Some(&ExpectedState::Hash(Blake3Hash::digest(CONTENT)))
        );
        let full = project(&fs, &input).unwrap();
        assert_eq!(full.graph.len(), 4);
        assert_eq!(
            full.documents
                .iter()
                .filter(|row| row.owner_revision.is_some())
                .count(),
            1
        );
        assert_eq!(
            full.documents
                .iter()
                .find(|row| row.path.as_str() == CONTENT_PATH)
                .unwrap()
                .raw_text
                .as_bytes(),
            CONTENT
        );
        assert!(full.links.len() >= 2);
        let graph = CatalogGraphValidator.validate(&fs, &input).unwrap();
        assert_eq!(graph.dependencies, validation.dependencies);
        assert_eq!(graph.control_manifest, validation.control_manifest);
    }

    #[test]
    fn validation_projection_preserves_malformed_duplicate_and_companion_diagnostics() {
        let (_temp, fs, mut input) = fixture();
        for (name, bytes) in [
            (
                "copy.md",
                include_bytes!("../../tests/fixtures/p08/a.md").to_vec(),
            ),
            (
                "malformed.md",
                b"---\nwiki_id: broken\nwiki_kind: entity\nwiki_status: [\n---\nReadable body\n"
                    .to_vec(),
            ),
        ] {
            input.overlay.push(ProposedTarget {
                path: path(name),
                bytes: Some(bytes),
            });
        }
        let assertion = input
            .documents
            .iter()
            .find(|document| document.path.as_str() == "assertions/use.md")
            .unwrap();
        let invalid_id = String::from_utf8(assertion.bytes.clone())
            .unwrap()
            .replace("wiki_subject_id: \"a\"", "wiki_subject_id: \"bad id\"");
        input.overlay.push(ProposedTarget {
            path: path("assertions/use.md"),
            bytes: Some(invalid_id.into_bytes()),
        });
        for closed in [false, true] {
            let validation = assert_differential(&fs, &input, closed);
            assert!(!validation.records.contains_key(&id("a")));
            assert!(!validation.records.contains_key(&id("use")));
            assert!(
                validation
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == ErrorCode::ReferenceAmbiguous)
            );
            assert!(
                validation
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.path.as_str() == "malformed.md")
            );
        }
    }

    #[test]
    fn validation_projection_preserves_failed_payload_read_dependencies() {
        for damaged in [
            "missing_original",
            "missing_content",
            "changed_original",
            "changed_content",
            "invalid_utf8",
        ] {
            let (temp, fs, input) = fixture();
            match damaged {
                "missing_original" => fs::remove_file(temp.path().join(ORIGINAL_PATH)).unwrap(),
                "missing_content" => fs::remove_file(temp.path().join(CONTENT_PATH)).unwrap(),
                "changed_original" => write(temp.path(), ORIGINAL_PATH, b"Changed original\n"),
                "changed_content" => write(temp.path(), CONTENT_PATH, b"Changed content\n"),
                "invalid_utf8" => write(temp.path(), CONTENT_PATH, &[0xff]),
                _ => unreachable!(),
            }
            let validation = assert_differential(&fs, &input, false);
            assert_eq!(
                validation.records[&id("revision_a")].eligibility,
                Eligibility::Invalid,
                "{damaged}"
            );
            for name in [ORIGINAL_PATH, CONTENT_PATH] {
                let expected = fs::read(temp.path().join(name))
                    .ok()
                    .map_or(ExpectedState::Absent, |bytes| {
                        ExpectedState::Hash(Blake3Hash::digest(bytes))
                    });
                assert_eq!(
                    dependency(&validation, name),
                    Some(&expected),
                    "{damaged}: {name}"
                );
            }
        }
    }

    #[test]
    fn validation_projection_preserves_overlay_deletion_and_closed_view_semantics() {
        let (_temp, fs, mut input) = fixture();
        // A closed view never falls through to the fixture's existing disk assets.
        let uncaptured = assert_differential(&fs, &input, true);
        assert_eq!(
            uncaptured.records[&id("revision_a")].eligibility,
            Eligibility::Invalid
        );
        assert_eq!(dependency(&uncaptured, ORIGINAL_PATH), None);
        assert_eq!(dependency(&uncaptured, CONTENT_PATH), None);
        for name in [ORIGINAL_PATH, CONTENT_PATH] {
            input.documents.push(ScanDocument {
                path: path(name),
                bytes: CONTENT.to_vec(),
                hash: Blake3Hash::digest(CONTENT),
            });
        }
        let captured = assert_differential(&fs, &input, true);
        assert_eq!(
            captured.records[&id("use")].eligibility,
            Eligibility::Current
        );
        // Proposed bytes and explicit absence take precedence over captured bytes.
        for bytes in [Some(b"Changed overlay\n".to_vec()), None] {
            input.overlay = vec![ProposedTarget {
                path: path(CONTENT_PATH),
                bytes: bytes.clone(),
            }];
            for closed in [false, true] {
                let validation = assert_differential(&fs, &input, closed);
                let expected = bytes.as_ref().map_or(ExpectedState::Absent, |bytes| {
                    ExpectedState::Hash(Blake3Hash::digest(bytes))
                });
                assert_eq!(dependency(&validation, CONTENT_PATH), Some(&expected));
                assert_eq!(
                    validation.records[&id("revision_a")].eligibility,
                    Eligibility::Invalid
                );
            }
        }
        input.overlay = vec![ProposedTarget {
            path: path("evidence/support.md"),
            bytes: None,
        }];
        for closed in [false, true] {
            let validation = assert_differential(&fs, &input, closed);
            assert!(!validation.records.contains_key(&id("support")));
            assert_ne!(
                validation.records[&id("use")].eligibility,
                Eligibility::Current
            );
        }
    }

    #[test]
    fn validation_projection_and_full_projection_reject_identical_bad_inputs() {
        let (_temp, fs, input) = fixture();
        for duplicate in [false, true] {
            let mut invalid = input.clone();
            if duplicate {
                invalid.documents.push(invalid.documents[0].clone());
            } else {
                invalid.documents[0].hash = Blake3Hash::digest(b"wrong scan hash");
            }
            for closed in [false, true] {
                let full = if closed {
                    project_closed(&fs, &invalid)
                } else {
                    project(&fs, &invalid)
                }
                .unwrap_err();
                let validation = if closed {
                    project_validation_closed(&fs, &invalid)
                } else {
                    project_validation(&fs, &invalid)
                }
                .unwrap_err();
                assert_eq!(
                    (full.code, full.message, full.details),
                    (validation.code, validation.message, validation.details)
                );
            }
        }
        let mut invalid_note = input;
        let bytes = b"---\nwiki_schema: \"1\"\nwiki_id: orphan\nwiki_kind: entity\nwiki_status: []\n---\nReadable\n".to_vec();
        invalid_note.documents.push(ScanDocument {
            path: path("orphan.md"),
            hash: Blake3Hash::digest(&bytes),
            bytes: bytes.clone(),
        });
        // An existing independent invalid envelope is tolerated unchanged.
        CatalogGraphValidator.validate(&fs, &invalid_note).unwrap();
        invalid_note.documents.pop();
        invalid_note.overlay.push(ProposedTarget {
            path: path("orphan.md"),
            bytes: Some(bytes),
        });
        let error = CatalogGraphValidator
            .validate(&fs, &invalid_note)
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::RecordInvalid);
        assert_eq!(
            error.message,
            "proposed adopted envelope is invalid: orphan.md"
        );
    }

    #[test]
    fn validation_projection_skips_normalization_on_source_payloads() {
        let (temp, fs, input) = fixture();
        let mut payload = CONTENT.to_vec();
        payload.resize(256 * 1024, b'x');
        write(temp.path(), CONTENT_PATH, &payload);
        NORMALIZATION_CALLS.with(|calls| calls.set(0));
        let validation = project_validation(&fs, &input).unwrap();
        assert_eq!(NORMALIZATION_CALLS.with(std::cell::Cell::get), 0);
        assert_eq!(
            dependency(&validation, CONTENT_PATH),
            Some(&ExpectedState::Hash(Blake3Hash::digest(&payload)))
        );
        NORMALIZATION_CALLS.with(|calls| calls.set(0));
        let full = project(&fs, &input).unwrap();
        assert!(NORMALIZATION_CALLS.with(std::cell::Cell::get) > 0);
        assert_eq!(
            full.documents
                .iter()
                .find(|row| row.path.as_str() == CONTENT_PATH)
                .unwrap()
                .raw_text
                .len(),
            payload.len()
        );
        NORMALIZATION_CALLS.with(|calls| calls.set(0));
        project_validation_closed(&fs, &input).unwrap();
        assert_eq!(NORMALIZATION_CALLS.with(std::cell::Cell::get), 0);
    }

    #[derive(Default)]
    struct RecordingSink {
        documents: BTreeMap<VaultRelativePath, DocumentRow>,
        graph: Vec<GraphRow>,
        links: Vec<LinkRow>,
    }
    impl RetrievalSink for RecordingSink {
        fn identity_claim(&mut self, _row: IdentityClaimRow) -> Result<()> {
            Ok(())
        }
        fn document(&mut self, row: DocumentRow) -> Result<()> {
            assert!(self.documents.insert(row.path.clone(), row).is_none());
            Ok(())
        }
        fn graph(&mut self, row: GraphRow) -> Result<()> {
            self.graph.push(row);
            Ok(())
        }
        fn link(&mut self, row: LinkRow) -> Result<()> {
            self.links.push(row);
            Ok(())
        }
    }

    #[test]
    fn streamed_sink_preserves_complete_collecting_projection_on_mixed_input() {
        let (_temp, fs, baseline) = fixture();
        for mixed in [false, true] {
            let mut input = baseline.clone();
            if mixed {
                input.overlay = vec![
                    ProposedTarget { path: path("plain.md"), bytes: Some(b"# Plain note\n[[entities/a]]\n".to_vec()) },
                    ProposedTarget { path: path("malformed.md"), bytes: Some(b"---\nwiki_id: broken\nwiki_kind: page\nwiki_status: [\n---\nMalformed readable note\n".to_vec()) },
                    ProposedTarget { path: path("copy.md"), bytes: Some(include_bytes!("../../tests/fixtures/p08/a.md").to_vec()) },
                ];
            }
            for closed in [false, true] {
                let mut captured = input.clone();
                if closed {
                    for name in [ORIGINAL_PATH, CONTENT_PATH] {
                        captured.documents.push(ScanDocument {
                            path: path(name),
                            bytes: CONTENT.to_vec(),
                            hash: Blake3Hash::digest(CONTENT),
                        });
                    }
                }
                let expected = if closed {
                    project_closed(&fs, &captured)
                } else {
                    project(&fs, &captured)
                }
                .unwrap();
                let mut sink = RecordingSink::default();
                let validation = project_with_sink(&fs, &captured, closed, &mut sink).unwrap();
                assert_eq!(
                    validation,
                    if closed {
                        project_validation_closed(&fs, &captured)
                    } else {
                        project_validation(&fs, &captured)
                    }
                    .unwrap()
                );
                let source = &sink.documents[&path(CONTENT_PATH)];
                assert_eq!(source.source_id, Some(id("source_a")));
                assert_eq!(source.owner_revision, Some(id("revision_a")));
                assert_eq!(source.raw_text.as_bytes(), CONTENT);
                assert_eq!(source.hash, Blake3Hash::digest(CONTENT));
                sink.links.sort_by(|a, b| {
                    (&a.from_path, a.byte_start).cmp(&(&b.from_path, b.byte_start))
                });
                let actual = CatalogProjection {
                    vault_id: validation.vault_id,
                    parser_fingerprint: validation.parser_fingerprint,
                    control_manifest: validation.control_manifest,
                    documents: sink.documents.into_values().collect(),
                    records: validation.records,
                    graph: sink.graph,
                    links: sink.links,
                    diagnostics: validation.diagnostics,
                    dependencies: validation.dependencies,
                };
                assert_eq!(actual, expected);
            }
        }
    }

    #[derive(Default)]
    struct OracleSink {
        documents: Vec<DocumentRow>,
        graph: Vec<GraphRow>,
        links: Vec<LinkRow>,
        events: Vec<String>,
    }
    impl RetrievalSink for OracleSink {
        fn identity_claim(&mut self, _row: IdentityClaimRow) -> Result<()> {
            Ok(())
        }
        fn document(&mut self, row: DocumentRow) -> Result<()> {
            self.events.push(format!("document:{}", row.path));
            self.documents.push(row);
            Ok(())
        }
        fn graph(&mut self, row: GraphRow) -> Result<()> {
            self.events.push(format!("graph:{}", row.target_id));
            self.graph.push(row);
            Ok(())
        }
        fn link(&mut self, row: LinkRow) -> Result<()> {
            self.events
                .push(format!("link:{}:{}", row.from_path, row.byte_start));
            self.links.push(row);
            Ok(())
        }
    }

    #[test]
    fn streamed_rows_match_literal_pre_sink_fixture_oracle() {
        let (_temp, fs, mut input) = fixture();
        // Fixed bytes and offsets, independently inspected against pre-sink
        // 609bdd9. The body link emits before the earlier typed companion link.
        const ASSERTION: &[u8] = b"---\ntitle: \"use\"\nwiki_id: \"use\"\nwiki_kind: \"assertion\"\nwiki_object_id: \"b\"\nwiki_predicate: \"uses\"\nwiki_schema: \"1\"\nwiki_status: \"accepted\"\nwiki_subject: \"[[entities/a]]\"\nwiki_subject_id: \"a\"\n---\nA uses B.\n[[entities/b]]\n";
        let assertion = input
            .documents
            .iter_mut()
            .find(|document| document.path == path("assertions/use.md"))
            .unwrap();
        assertion.bytes = ASSERTION.to_vec();
        assertion.hash = Blake3Hash::digest(ASSERTION);
        assert_eq!(&ASSERTION[154..168], b"[[entities/a]]");
        assert_eq!(&ASSERTION[205..219], b"[[entities/b]]");
        let page = &input
            .documents
            .iter()
            .find(|document| document.path == path("page.md"))
            .unwrap()
            .bytes;
        assert_eq!(&page[143..157], b"[[entities/a]]");

        // Expected display fields are literals, not products of normalization,
        // parsing, resolution, eligibility or collecting-sink helpers. Raw bytes
        // and their hashes come only from the authoritative fixture input.
        type LiteralDocument<'a> = (
            &'a str,
            &'a str,
            RecordKind,
            &'a str,
            &'a str,
            Eligibility,
            &'a [&'a str],
        );
        let specifications: [LiteralDocument<'_>; 10] = [
            (
                "WIKI.md",
                "vault_projection",
                RecordKind::Vault,
                "",
                "Wiki\n",
                Eligibility::Current,
                &[],
            ),
            (
                "assertions/use.md",
                "use",
                RecordKind::Assertion,
                "",
                "A uses B.\n[[entities/b]]\n",
                Eligibility::Current,
                &["current_support", "disputed"],
            ),
            (
                "entities/a.md",
                "a",
                RecordKind::Entity,
                "",
                "Identity remains searchable independently of this unsupported description.\n",
                Eligibility::Unsupported,
                &["description_without_support"],
            ),
            (
                "entities/b.md",
                "b",
                RecordKind::Entity,
                "",
                "Identity remains searchable independently of this unsupported description.\n",
                Eligibility::Unsupported,
                &["description_without_support"],
            ),
            (
                "entities/c.md",
                "c",
                RecordKind::Entity,
                "",
                "Identity remains searchable independently of this unsupported description.\n",
                Eligibility::Unsupported,
                &["description_without_support"],
            ),
            (
                "evidence/opposition.md",
                "opposition",
                RecordKind::Evidence,
                "",
                "",
                Eligibility::Current,
                &[],
            ),
            (
                "evidence/support.md",
                "support",
                RecordKind::Evidence,
                "",
                "",
                Eligibility::Current,
                &[],
            ),
            (
                "page.md",
                "page",
                RecordKind::Page,
                "Supported page",
                "Supported page\n[[entities/a]]\n",
                Eligibility::Current,
                &[],
            ),
            (
                "sources/source_a/revisions/revision_a/revision.md",
                "revision_a",
                RecordKind::Revision,
                "",
                "Revision\n",
                Eligibility::Current,
                &[],
            ),
            (
                "sources/source_a/source.md",
                "source_a",
                RecordKind::Source,
                "",
                "Source\n",
                Eligibility::Current,
                &[],
            ),
        ];
        let canonical_documents: Vec<_> = specifications
            .into_iter()
            .map(
                |(name, record_id, kind, headings, body, eligibility, reasons)| {
                    let bytes = &input
                        .documents
                        .iter()
                        .find(|document| document.path.as_str() == name)
                        .unwrap()
                        .bytes;
                    DocumentRow {
                        path: path(name),
                        hash: Blake3Hash::digest(bytes),
                        record_id: Some(id(record_id)),
                        kind: Some(kind),
                        title: record_id.into(),
                        aliases: vec![],
                        headings: headings.into(),
                        tags: vec![],
                        body: body.into(),
                        raw_text: String::from_utf8(bytes.clone()).unwrap(),
                        source_id: None,
                        owner_revision: None,
                        eligibility,
                        reasons: reasons.iter().map(|reason| (*reason).to_owned()).collect(),
                    }
                },
            )
            .collect();
        let source_document = DocumentRow {
            path: path(CONTENT_PATH),
            hash: Blake3Hash::digest(CONTENT),
            record_id: None,
            kind: None,
            title: "revision_a".into(),
            aliases: vec![],
            headings: "Captured source".into(),
            tags: vec![],
            body: "Captured source\nA uses B.\n".into(),
            raw_text: "# Captured source\nA uses B.\n".into(),
            source_id: Some(id("source_a")),
            owner_revision: Some(id("revision_a")),
            eligibility: Eligibility::Current,
            reasons: vec![],
        };
        let mut expected_graph: Vec<_> = ["a", "b", "c"]
            .into_iter()
            .map(|name| GraphRow {
                target_id: id(name),
                target_kind: RecordKind::Entity,
                name: name.into(),
                aliases: vec![],
                endpoints: String::new(),
                predicate: String::new(),
                qualifiers: "{}".into(),
                description: String::new(),
            })
            .collect();
        expected_graph.push(GraphRow {
            target_id: id("use"),
            target_kind: RecordKind::Assertion,
            name: String::new(),
            aliases: vec![],
            endpoints: "a b".into(),
            predicate: "uses".into(),
            qualifiers: "{\"wiki_modality\":\"asserted\",\"wiki_negated\":false}".into(),
            description: "A uses B.\n[[entities/b]]\n".into(),
        });
        const RESOLVED_A: &str = "Resolved { id: RecordId(\"a\"), path: VaultRelativePath(\"entities/a.md\"), fragment: None, companion_stale: false }";
        const RESOLVED_B: &str = "Resolved { id: RecordId(\"b\"), path: VaultRelativePath(\"entities/b.md\"), fragment: None, companion_stale: false }";
        let typed_link = LinkRow {
            from_path: path("assertions/use.md"),
            byte_start: 154,
            target_id: Some(id("a")),
            target_path: Some(path("entities/a.md")),
            resolution: RESOLVED_A.into(),
        };
        let body_link = LinkRow {
            from_path: path("assertions/use.md"),
            byte_start: 205,
            target_id: Some(id("b")),
            target_path: Some(path("entities/b.md")),
            resolution: RESOLVED_B.into(),
        };
        let page_link = LinkRow {
            from_path: path("page.md"),
            byte_start: 143,
            target_id: Some(id("a")),
            target_path: Some(path("entities/a.md")),
            resolution: RESOLVED_A.into(),
        };
        let expected_events = [
            "document:WIKI.md",
            "document:assertions/use.md",
            "link:assertions/use.md:205",
            "link:assertions/use.md:154",
            "document:entities/a.md",
            "document:entities/b.md",
            "document:entities/c.md",
            "document:evidence/opposition.md",
            "document:evidence/support.md",
            "document:page.md",
            "link:page.md:143",
            "document:sources/source_a/revisions/revision_a/revision.md",
            "document:sources/source_a/source.md",
            "graph:a",
            "graph:b",
            "graph:c",
            "document:sources/source_a/revisions/revision_a/content.md",
            "graph:use",
        ];
        for closed in [false, true] {
            let mut captured = input.clone();
            if closed {
                for name in [ORIGINAL_PATH, CONTENT_PATH] {
                    captured.documents.push(ScanDocument {
                        path: path(name),
                        bytes: CONTENT.to_vec(),
                        hash: Blake3Hash::digest(CONTENT),
                    });
                }
            }
            let mut sink = OracleSink::default();
            project_with_sink(&fs, &captured, closed, &mut sink).unwrap();
            assert_eq!(
                sink.events.iter().map(String::as_str).collect::<Vec<_>>(),
                expected_events
            );
            let mut expected_stream_documents = canonical_documents.clone();
            expected_stream_documents.push(source_document.clone());
            assert_eq!(sink.documents, expected_stream_documents);
            assert_eq!(sink.graph, expected_graph);
            assert_eq!(
                sink.links,
                vec![body_link.clone(), typed_link.clone(), page_link.clone()]
            );
            // Collector ordering has independent fixed expectations, not a sort
            // applied to the actual callback rows to manufacture its oracle.
            let projection = if closed {
                project_closed(&fs, &captured)
            } else {
                project(&fs, &captured)
            }
            .unwrap();
            let mut expected_sorted_documents = canonical_documents.clone();
            expected_sorted_documents.insert(8, source_document.clone());
            assert_eq!(projection.documents, expected_sorted_documents);
            assert_eq!(projection.graph, expected_graph);
            assert_eq!(
                projection.links,
                vec![typed_link.clone(), body_link.clone(), page_link.clone()]
            );
        }
    }

    struct FailingSink {
        stage: &'static str,
        failed: bool,
        attempts: usize,
        source_attempts: usize,
    }
    impl FailingSink {
        fn step(&mut self, stage: &'static str) -> Result<()> {
            assert!(!self.failed, "callback invoked after sink error");
            self.attempts += 1;
            if stage == self.stage {
                self.failed = true;
                let mut error =
                    WikiError::new(ErrorCode::BudgetExceeded, "injected retrieval sink error");
                error.details = serde_json::json!({"stage":stage});
                return Err(error);
            }
            Ok(())
        }
    }
    impl RetrievalSink for FailingSink {
        fn identity_claim(&mut self, _row: IdentityClaimRow) -> Result<()> {
            Ok(())
        }
        fn document(&mut self, row: DocumentRow) -> Result<()> {
            if row.owner_revision.is_some() {
                self.source_attempts += 1;
                assert_eq!(row.source_id, Some(id("source_a")));
                assert_eq!(row.owner_revision, Some(id("revision_a")));
                assert_eq!(row.path, path(CONTENT_PATH));
                assert_eq!(row.hash, Blake3Hash::digest(CONTENT));
                assert_eq!(row.raw_text.as_bytes(), CONTENT);
                self.step("source_document")
            } else {
                self.step("document")
            }
        }
        fn graph(&mut self, _row: GraphRow) -> Result<()> {
            self.step("graph")
        }
        fn link(&mut self, _row: LinkRow) -> Result<()> {
            self.step("link")
        }
    }

    #[test]
    fn streamed_sink_errors_abort_without_complete_validation_authority() {
        let (_temp, fs, input) = fixture();
        for closed in [false, true] {
            let mut captured = input.clone();
            if closed {
                for name in [ORIGINAL_PATH, CONTENT_PATH] {
                    captured.documents.push(ScanDocument {
                        path: path(name),
                        bytes: CONTENT.to_vec(),
                        hash: Blake3Hash::digest(CONTENT),
                    });
                }
            }
            for stage in ["document", "graph", "link", "source_document"] {
                let mut sink = FailingSink {
                    stage,
                    failed: false,
                    attempts: 0,
                    source_attempts: 0,
                };
                let error = project_with_sink(&fs, &captured, closed, &mut sink).unwrap_err();
                assert_eq!(error.code, ErrorCode::BudgetExceeded);
                assert_eq!(error.message, "injected retrieval sink error");
                assert_eq!(error.details, serde_json::json!({"stage":stage}));
                assert!(sink.failed);
                if stage == "document" {
                    assert_eq!(sink.attempts, 1);
                }
                if stage == "source_document" {
                    assert_eq!(sink.source_attempts, 1);
                }
            }
        }
    }

    /// Root runs this bounded diagnostic in a fresh process per mode/size and
    /// captures process RSS externally. It is not a capacity benchmark.
    #[test]
    #[ignore = "bounded full-versus-validation resource diagnostic"]
    fn validation_projection_resource_diagnostic() {
        let mode = std::env::var("LWIKI_PROJECTION_DIAGNOSTIC_MODE").unwrap();
        assert!(matches!(mode.as_str(), "full" | "validation"));
        let size: usize = std::env::var("LWIKI_PROJECTION_DIAGNOSTIC_BYTES")
            .unwrap()
            .parse()
            .unwrap();
        assert!((1024..=8 * 1024 * 1024).contains(&size));
        let (temp, fs, input) = fixture();
        let mut payload = CONTENT.to_vec();
        payload.resize(size, b'x');
        write(temp.path(), CONTENT_PATH, &payload);
        write(temp.path(), ORIGINAL_PATH, &payload);
        let revision_path = "sources/source_a/revisions/revision_a/revision.md";
        let old = fs::read(temp.path().join(revision_path)).unwrap();
        let updated = String::from_utf8(old).unwrap().replace(
            Blake3Hash::digest(CONTENT).as_str(),
            Blake3Hash::digest(&payload).as_str(),
        );
        write(temp.path(), revision_path, updated.as_bytes());
        let payload_hash = Blake3Hash::digest(&payload);
        drop(payload);
        let input = scan_input(&fs, &input.vault_id).unwrap();
        NORMALIZATION_CALLS.with(|calls| calls.set(0));
        let started = std::time::Instant::now();
        let (records, source_rows, retained_retrieval_bytes) = if mode == "full" {
            let result = project(&fs, &input).unwrap();
            assert_eq!(result.records[&id("use")].eligibility, Eligibility::Current);
            (
                result.records.len(),
                result
                    .documents
                    .iter()
                    .filter(|row| row.owner_revision.is_some())
                    .count(),
                result
                    .documents
                    .iter()
                    .map(|row| row.raw_text.len() + row.body.len() + row.headings.len())
                    .sum::<usize>(),
            )
        } else {
            let result = project_validation(&fs, &input).unwrap();
            assert_eq!(result.records[&id("use")].eligibility, Eligibility::Current);
            assert_eq!(
                dependency(&result, CONTENT_PATH),
                Some(&ExpectedState::Hash(payload_hash))
            );
            (result.records.len(), 0, 0)
        };
        let elapsed = started.elapsed().as_secs_f64();
        let normalization_calls = NORMALIZATION_CALLS.with(std::cell::Cell::get);
        if mode == "validation" {
            assert_eq!(normalization_calls, 0);
        }
        println!(
            "{}",
            json!({"mode":mode,"source_payload_bytes":size,"records":records,"source_rows":source_rows,"retained_retrieval_bytes":retained_retrieval_bytes,"normalization_calls":normalization_calls,"elapsed_seconds":elapsed,"clock":"std::time::Instant process-local monotonic"})
        );
    }

    #[test]
    fn streamed_manifest_matches_old_framing_for_empty_unicode_and_binary_notes() {
        fn old_hash(notes: &BTreeMap<VaultRelativePath, ParsedNote>) -> Blake3Hash {
            let mut bytes = b"lwiki-canonical-control-v1\0".to_vec();
            for (path, note) in notes {
                bytes.extend_from_slice(&(path.as_str().len() as u64).to_le_bytes());
                bytes.extend_from_slice(path.as_str().as_bytes());
                bytes.extend_from_slice(&(note.raw.len() as u64).to_le_bytes());
                bytes.extend_from_slice(&note.raw);
            }
            Blake3Hash::digest(bytes)
        }
        let mut notes = BTreeMap::new();
        assert_eq!(manifest_hash(&notes), old_hash(&notes));
        for (name, bytes) in [
            ("empty.md", b"".as_slice()),
            (
                "pages/\u{00e9}\u{65e5}.md",
                "# \u{65e5}\u{672c}\n\u{00e9}\u{1f642}\n".as_bytes(),
            ),
            ("binary.md", &[0xff, 0, 0x80]),
            (
                "malformed.md",
                b"---\nwiki_id: duplicate\nwiki_id: duplicate\n---\n",
            ),
        ] {
            notes.insert(path(name), parse_note(bytes));
            assert_eq!(manifest_hash(&notes), old_hash(&notes));
        }
        let mut input = ValidationInput {
            vault_id: id("vault_manifest"),
            documents: notes
                .iter()
                .map(|(path, note)| ScanDocument {
                    path: path.clone(),
                    bytes: note.raw.clone(),
                    hash: note.source_hash.clone(),
                })
                .collect(),
            overlay: vec![],
        };
        input.documents.reverse();
        assert_eq!(
            manifest_hash(&input_notes(&input).unwrap()),
            old_hash(&notes)
        );
        let same_size = b"different same-sized value".to_vec();
        let before = same_size.iter().map(|_| b'x').collect::<Vec<_>>();
        input.documents.push(ScanDocument {
            path: path("edit.md"),
            hash: Blake3Hash::digest(&before),
            bytes: before,
        });
        let before_notes = input_notes(&input).unwrap();
        input.overlay.push(ProposedTarget {
            path: path("edit.md"),
            bytes: Some(same_size),
        });
        input.overlay.push(ProposedTarget {
            path: path("empty.md"),
            bytes: None,
        });
        let after_notes = input_notes(&input).unwrap();
        assert_eq!(manifest_hash(&after_notes), old_hash(&after_notes));
        assert_ne!(manifest_hash(&after_notes), manifest_hash(&before_notes));
        assert!(!after_notes.contains_key(&path("empty.md")));
    }
}

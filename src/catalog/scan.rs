//! Deterministic canonical projection. This module performs no extraction or model calls.
use super::types::*;
use crate::{
    changes::{ReadDependency, ScanDocument, ValidationInput},
    domain::{
        Blake3Hash, Eligibility, ErrorCode, RecordId, RecordKind, Result, VaultRelativePath,
        WikiError,
    },
    records::{
        LinkResolution, ParsedNote, RegistryEntry, extract_links, parse_note, resolve_typed,
        resolve_untyped,
    },
    sources::{SourceView, revision::canonical_path},
    vault::{ExpectedState, VaultFs},
};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use std::collections::{BTreeMap, BTreeSet};

/// Cache identity binds every semantic adapter, not merely frontmatter parsing.
pub fn parser_fingerprint() -> Blake3Hash {
    Blake3Hash::digest(format!(
        "{};catalog-v1;canonical-membership-bytewise-v1;source-original-content-span-quote-fence-structural-v2;typed-id-companion-v1;decisions-explicit-conflict-cycle-v1;eligibility-full-note-transitive-v1;markdown-lexical-events-v1;graph-readable-directed-endpoints-v1;unicode61 remove_diacritics 2;no-stemming",
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

fn readable_ids(note: &ParsedNote) -> BTreeSet<RecordId> {
    if let Some(value) = note
        .fields
        .as_ref()
        .and_then(|f| f.get("wiki_id"))
        .and_then(serde_json::Value::as_str)
    {
        return RecordId::new(value).into_iter().collect();
    }
    isolated_fields(note)
        .iter()
        .filter_map(|fields| fields.get("wiki_id"))
        .filter_map(serde_json::Value::as_str)
        .filter_map(|value| RecordId::new(value).ok())
        .collect()
}

/// Bounded strict parsing of independent top-level fields is only a reservation
/// aid for malformed envelopes. These fields never create adopted records.
pub(crate) fn isolated_fields(note: &ParsedNote) -> Vec<BTreeMap<String, serde_json::Value>> {
    let Some(text) = note.literal_text() else {
        return Vec::new();
    };
    let mut lines = text.strip_prefix('\u{feff}').unwrap_or(text).lines();
    if lines.next() != Some("---") {
        return Vec::new();
    }
    let envelope: Vec<_> = lines.take_while(|line| *line != "---").collect();
    let mut fields = Vec::new();
    let mut index = 0;
    while index < envelope.len() {
        let line = envelope[index];
        if line.is_empty() || line.starts_with([' ', '\t']) {
            index += 1;
            continue;
        }
        let start = index;
        index += 1;
        while index < envelope.len()
            && (envelope[index].is_empty() || envelope[index].starts_with([' ', '\t']))
        {
            index += 1;
        }
        let candidate = envelope[start..index].join("\n");
        if candidate.len() > crate::records::ParseLimits::default().max_envelope_bytes {
            continue;
        }
        let candidate = parse_note(format!("---\n{candidate}\n---\n").as_bytes());
        if let Some(parsed) = candidate.fields {
            fields.push(parsed);
        }
    }
    fields
}

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
    let notes = input_notes(input)?;
    let mut memberships: BTreeMap<RecordId, Vec<VaultRelativePath>> = BTreeMap::new();
    for (path, note) in &notes {
        for id in readable_ids(note) {
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
    let source_view = SourceView::from_input(fs, input)?;
    super::eligibility::compute(&source_view, &notes, &mut records, &mut diagnostics)?;
    let registry: Vec<_> = records
        .values()
        .map(|row| RegistryEntry {
            id: row.record.id().clone(),
            kind: row.record.kind(),
            path: row.path.clone(),
            aliases: list(&row.record, "aliases"),
        })
        .collect();
    let mut documents = Vec::new();
    let mut links = Vec::new();
    let mut graph = Vec::new();
    let mut dependencies: BTreeMap<VaultRelativePath, ExpectedState> = notes
        .iter()
        .map(|(p, n)| (p.clone(), ExpectedState::Hash(n.source_hash.clone())))
        .collect();
    for (path, note) in &notes {
        let row = note
            .canonical
            .as_ref()
            .and_then(|record| records.get(record.id()))
            .filter(|row| &row.path == path);
        let raw_text = note.literal_text().unwrap_or_default().to_owned();
        let body = std::str::from_utf8(note.body()).unwrap_or_default();
        let excluded = row.is_some_and(|r| {
            matches!(
                r.record.kind(),
                RecordKind::Evidence | RecordKind::Extraction | RecordKind::ExtractionPacket
            )
        });
        let (headings, normalized) = if excluded {
            (String::new(), String::new())
        } else {
            normalized_markdown(body)
        };
        let title = row.map_or_else(
            || first_heading(body).unwrap_or_else(|| path.as_str().to_owned()),
            |r| r.record.title().to_owned(),
        );
        documents.push(DocumentRow {
            path: path.clone(),
            hash: note.source_hash.clone(),
            record_id: row.map(|r| r.record.id().clone()),
            kind: row.map(|r| r.record.kind()),
            title,
            aliases: row.map_or_else(Vec::new, |r| list(&r.record, "aliases")),
            headings,
            tags: row.map_or_else(Vec::new, |r| list(&r.record, "tags")),
            body: normalized,
            raw_text,
            source_id: None,
            owner_revision: None,
            eligibility: row.map_or_else(
                || {
                    if note.canonical.is_some()
                        || readable_id(note).is_some()
                        || !note.diagnostics.is_empty()
                    {
                        Eligibility::Invalid
                    } else {
                        Eligibility::Current
                    }
                },
                |r| r.eligibility,
            ),
            reasons: row.map_or_else(
                || {
                    if note.canonical.is_some() || !note.diagnostics.is_empty() {
                        vec!["unadopted_or_invalid".into()]
                    } else {
                        vec!["note_text".into()]
                    }
                },
                |r| r.reasons.clone(),
            ),
        });
        let body_offset = note.raw.len() - note.body().len();
        for link in extract_links(body) {
            let resolution = resolve_untyped(&registry, &link.destination);
            let (target_id, target_path) = match &resolution {
                LinkResolution::Resolved { id, path, .. } => (Some(id.clone()), Some(path.clone())),
                _ => (None, None),
            };
            links.push(LinkRow {
                from_path: path.clone(),
                byte_start: (body_offset + link.range.start) as u64,
                target_id,
                target_path,
                resolution: format!("{resolution:?}"),
            });
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
                let target = RecordId::new(value)?;
                let resolution = resolve_typed(&registry, &target, kind, Some(destination));
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
                links.push(LinkRow {
                    from_path: path.clone(),
                    byte_start: start as u64,
                    target_id,
                    target_path,
                    resolution: format!("{resolution:?}"),
                });
            }
        }
    }
    for row in records.values() {
        for dependency in &row.dependencies {
            dependencies.insert(dependency.path.clone(), dependency.expected.clone());
        }
        let record = &row.record;
        if matches!(record.kind(), RecordKind::Entity | RecordKind::Assertion) {
            graph.push(GraphRow {
                target_id: record.id().clone(),
                target_kind: record.kind(),
                name: if record.kind() == RecordKind::Entity {
                    record.title().into()
                } else {
                    String::new()
                },
                aliases: list(record, "aliases"),
                endpoints: [
                    record.string("wiki_subject_id"),
                    record.string("wiki_object_id"),
                ]
                .into_iter()
                .flatten()
                .filter_map(|id| RecordId::new(id).ok().and_then(|id| records.get(&id)))
                .filter(|row| row.record.kind() == RecordKind::Entity)
                .flat_map(|row| {
                    std::iter::once(row.record.title().to_owned())
                        .chain(list(&row.record, "aliases"))
                })
                .collect::<Vec<_>>()
                .join(" "),
                predicate: record.string("wiki_predicate").unwrap_or_default().into(),
                qualifiers: serde_json::to_string(&qualifiers(record))
                    .map_err(|e| WikiError::invalid(e.to_string()))?,
                description: if record.kind() == RecordKind::Entity
                    && row.description_eligibility != Some(Eligibility::Current)
                {
                    String::new()
                } else {
                    record
                        .string("description")
                        .map(str::to_owned)
                        .unwrap_or_else(|| {
                            notes
                                .get(&row.path)
                                .map(|n| {
                                    normalized_markdown(
                                        std::str::from_utf8(n.body()).unwrap_or_default(),
                                    )
                                    .1
                                })
                                .unwrap_or_default()
                        })
                },
            });
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
                if let Ok(content) = source_view.read(&path, &mut deps)
                    && let Ok(raw_text) = String::from_utf8(content)
                {
                    let (headings, body) = normalized_markdown(&raw_text);
                    documents.push(DocumentRow {
                        path,
                        hash: Blake3Hash::digest(raw_text.as_bytes()),
                        record_id: None,
                        kind: None,
                        title: record.title().into(),
                        aliases: vec![],
                        headings,
                        tags: vec![],
                        body,
                        raw_text,
                        source_id: Some(source_id),
                        owner_revision: Some(record.id().clone()),
                        eligibility: row.eligibility,
                        reasons: row.reasons.clone(),
                    });
                }
            }
            // Failed verification also contributes every byte observed before failure.
            dependencies.extend(deps);
        }
    }
    documents.sort_by(|a, b| a.path.as_str().as_bytes().cmp(b.path.as_str().as_bytes()));
    links.sort_by(|a, b| (&a.from_path, a.byte_start).cmp(&(&b.from_path, b.byte_start)));
    diagnostics.sort_by(|a, b| {
        (&a.path, format!("{:?}", a.code), a.details.to_string()).cmp(&(
            &b.path,
            format!("{:?}", b.code),
            b.details.to_string(),
        ))
    });
    let control_manifest = manifest_hash(&notes);
    Ok(CatalogProjection {
        vault_id: input.vault_id.clone(),
        parser_fingerprint: parser_fingerprint(),
        control_manifest,
        documents,
        records,
        graph,
        links,
        diagnostics,
        dependencies: dependencies
            .into_iter()
            .map(|(path, expected)| ReadDependency { path, expected })
            .collect(),
    })
}

fn manifest_hash(notes: &BTreeMap<VaultRelativePath, ParsedNote>) -> Blake3Hash {
    let mut bytes = b"lwiki-canonical-control-v1\0".to_vec();
    for (path, note) in notes {
        bytes.extend_from_slice(&(path.as_str().len() as u64).to_le_bytes());
        bytes.extend_from_slice(path.as_str().as_bytes());
        bytes.extend_from_slice(&(note.raw.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&note.raw);
    }
    Blake3Hash::digest(bytes)
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

fn qualifiers(record: &crate::domain::CanonicalRecord) -> BTreeMap<String, serde_json::Value> {
    let mut fields = BTreeMap::new();
    if record.kind() != RecordKind::Assertion {
        return fields;
    }
    for key in [
        "wiki_literal_type",
        "wiki_literal_value",
        "wiki_property",
        "wiki_unit",
        "wiki_valid_from",
        "wiki_valid_until",
    ] {
        if let Some(value) = record.field(key) {
            fields.insert(key.to_owned(), value.clone());
        }
    }
    fields.insert(
        "wiki_negated".into(),
        record
            .field("wiki_negated")
            .cloned()
            .unwrap_or(false.into()),
    );
    fields.insert(
        "wiki_modality".into(),
        record
            .field("wiki_modality")
            .cloned()
            .unwrap_or("asserted".into()),
    );
    fields
}

pub(crate) fn normalized_markdown(markdown: &str) -> (String, String) {
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
fn first_heading(body: &str) -> Option<String> {
    normalized_markdown(body)
        .0
        .lines()
        .next()
        .map(str::to_owned)
}

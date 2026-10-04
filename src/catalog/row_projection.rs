//! Pure rendering shared by full projection and bounded delta publication.
//!
//! These functions confer no identity, eligibility or byte-verification proof.
//! Callers supply adopted records and bounded, authenticated input as appropriate
//! to their workflow. Full projection also represents invalid readable content.
use super::{
    scan::{isolated_fields, list, normalized_markdown, readable_id},
    types::{DocumentRow, GraphRow, RecordRow},
};
use crate::{
    domain::{
        Blake3Hash, CanonicalRecord, Eligibility, RecordId, RecordKind, Result, VaultRelativePath,
        WikiError,
    },
    records::ParsedNote,
};
use std::collections::BTreeMap;

pub(crate) fn canonical_document(
    path: &VaultRelativePath,
    note: &ParsedNote,
    row: Option<&RecordRow>,
) -> DocumentRow {
    let raw_text = note.literal_text().unwrap_or_default().to_owned();
    let body = std::str::from_utf8(note.body()).unwrap_or_default();
    let bookkeeping = declared_bookkeeping_kind(note);
    let excluded = bookkeeping.is_some_and(|kind| kind != RecordKind::Decision)
        || row.is_some_and(|r| {
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
    DocumentRow {
        path: path.clone(),
        hash: note.source_hash.clone(),
        record_id: row.map(|r| r.record.id().clone()),
        kind: row.map(|r| r.record.kind()).or(bookkeeping),
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
    }
}

/// Render supplied UTF-8 capture bytes using the immutable revision title.
/// Incremental callers must verify the content/source/revision binding before
/// invoking this; the complete projector also emits invalid readable captures.
pub(crate) fn captured_content_document(
    path: VaultRelativePath,
    source_id: RecordId,
    revision: &RecordRow,
    raw_text: String,
) -> DocumentRow {
    let (headings, body) = normalized_markdown(&raw_text);
    DocumentRow {
        path,
        hash: Blake3Hash::digest(raw_text.as_bytes()),
        record_id: None,
        kind: None,
        title: revision.record.title().into(),
        aliases: vec![],
        headings,
        tags: vec![],
        body,
        raw_text,
        source_id: Some(source_id),
        owner_revision: Some(revision.record.id().clone()),
        eligibility: revision.eligibility,
        reasons: revision.reasons.clone(),
    }
}

/// Endpoint slots are subject then object; callers need at most two lookups.
/// Missing or non-entity endpoints contribute no text, as in full projection.
pub(crate) fn graph_row(
    row: &RecordRow,
    note: Option<&ParsedNote>,
    endpoints: [Option<&CanonicalRecord>; 2],
) -> Result<GraphRow> {
    let record = &row.record;
    Ok(GraphRow {
        target_id: record.id().clone(),
        target_kind: record.kind(),
        name: if record.kind() == RecordKind::Entity {
            record.title().into()
        } else {
            String::new()
        },
        aliases: list(record, "aliases"),
        endpoints: endpoints
            .into_iter()
            .flatten()
            .filter(|record| record.kind() == RecordKind::Entity)
            .flat_map(|record| {
                std::iter::once(record.title().to_owned()).chain(list(record, "aliases"))
            })
            .collect::<Vec<_>>()
            .join(" "),
        predicate: record.string("wiki_predicate").unwrap_or_default().into(),
        qualifiers: serde_json::to_string(&qualifiers(record))
            .map_err(|error| WikiError::invalid(error.to_string()))?,
        description: if record.kind() == RecordKind::Entity
            && row.description_eligibility != Some(Eligibility::Current)
        {
            String::new()
        } else {
            record
                .string("description")
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    note.map(|note| {
                        normalized_markdown(std::str::from_utf8(note.body()).unwrap_or_default()).1
                    })
                    .unwrap_or_default()
                })
        },
    })
}

fn declared_bookkeeping_kind(note: &ParsedNote) -> Option<RecordKind> {
    let bookkeeping = |kind| {
        matches!(
            kind,
            RecordKind::Decision
                | RecordKind::ExtractionPacket
                | RecordKind::Extraction
                | RecordKind::Run
                | RecordKind::RunEvent
                | RecordKind::Change
        )
    };
    let declared = note
        .canonical
        .as_ref()
        .map(|record| record.kind())
        .or_else(|| {
            note.fields.as_ref().and_then(|fields| {
                fields
                    .get("wiki_kind")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|kind| kind.parse().ok())
            })
        });
    declared.filter(|kind| bookkeeping(*kind)).or_else(|| {
        isolated_fields(note).iter().find_map(|fields| {
            fields
                .get("wiki_kind")
                .and_then(serde_json::Value::as_str)
                .and_then(|kind| kind.parse().ok())
                .filter(|kind| bookkeeping(*kind))
        })
    })
}

fn qualifiers(record: &CanonicalRecord) -> BTreeMap<String, serde_json::Value> {
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

fn first_heading(body: &str) -> Option<String> {
    normalized_markdown(body)
        .0
        .lines()
        .next()
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        catalog::scan,
        records::{edit_note, parse_note},
        sources::{
            CaptureRequest, ExtractionInput, SourceOrigin, SourceStore,
            revision::{common, record_bytes},
        },
        vault::{VaultFs, VaultRoot},
    };
    use serde_json::json;
    use std::fs;

    fn path(value: &str) -> VaultRelativePath {
        VaultRelativePath::new(value).unwrap()
    }

    fn row(record: CanonicalRecord, bytes: &[u8], location: &str) -> RecordRow {
        RecordRow {
            authored_status: record.string("wiki_status").map(str::to_owned),
            record,
            path: path(location),
            hash: Blake3Hash::digest(bytes),
            eligibility: Eligibility::Current,
            reasons: vec![],
            identity_eligibility: None,
            description_eligibility: None,
            disputed: false,
            dependencies: vec![],
        }
    }

    #[test]
    fn plain_and_unadopted_bookkeeping_have_independent_document_oracles() {
        let plain = "# Café\n\nA `code` **fact**.\n".as_bytes();
        let note = parse_note(plain);
        let expected = DocumentRow {
            path: path("plain.md"),
            hash: Blake3Hash::digest(plain),
            record_id: None,
            kind: None,
            title: "Café".into(),
            aliases: vec![],
            headings: "Café".into(),
            tags: vec![],
            body: "Café\nA code fact.\n".into(),
            raw_text: String::from_utf8(plain.to_vec()).unwrap(),
            source_id: None,
            owner_revision: None,
            eligibility: Eligibility::Current,
            reasons: vec!["note_text".into()],
        };
        assert_eq!(canonical_document(&path("plain.md"), &note, None), expected);
        for (kind, excluded) in [("run", true), ("decision", false)] {
            let raw = format!(
                "---\nwiki_schema: \"2\"\nwiki_id: copied_history\nwiki_kind: {kind}\ntitle: Unadopted metadata\n---\n# Copied outcome\n\nCopied **text**.\n"
            );
            let note = parse_note(raw.as_bytes());
            assert!(note.canonical.is_none());
            let expected = DocumentRow {
                path: path("copied.md"),
                hash: Blake3Hash::digest(raw.as_bytes()),
                record_id: None,
                kind: Some(if excluded {
                    RecordKind::Run
                } else {
                    RecordKind::Decision
                }),
                title: "Copied outcome".into(),
                aliases: vec![],
                headings: if excluded {
                    String::new()
                } else {
                    "Copied outcome".into()
                },
                tags: vec![],
                body: if excluded {
                    String::new()
                } else {
                    "Copied outcome\nCopied text.\n".into()
                },
                raw_text: raw,
                source_id: None,
                owner_revision: None,
                eligibility: Eligibility::Invalid,
                reasons: vec!["unadopted_or_invalid".into()],
            };
            assert_eq!(
                canonical_document(&path("copied.md"), &note, None),
                expected
            );
        }
    }

    #[test]
    fn graph_keeps_endpoint_order_qualifiers_and_entity_description_channel() {
        let subject_id = RecordId::new("entity_subject").unwrap();
        let object_id = RecordId::new("entity_object").unwrap();
        let make_entity = |id: &RecordId, title: &str, aliases: Vec<&str>| {
            let mut fields = common(id, RecordKind::Entity, title);
            fields.extend(BTreeMap::from([
                ("wiki_status".into(), json!("active")),
                ("wiki_entity_type".into(), json!("person")),
                ("aliases".into(), json!(aliases)),
            ]));
            CanonicalRecord::new(fields).unwrap()
        };
        let subject = make_entity(&subject_id, "Mira", vec!["M", "café"]);
        let object = make_entity(&object_id, "Cedar", vec!["C"]);
        let mut fields = common(
            &RecordId::new("assertion_use").unwrap(),
            RecordKind::Assertion,
            "Use",
        );
        fields.extend(BTreeMap::from([
            ("wiki_status".into(), json!("proposed")),
            ("wiki_subject_id".into(), json!(subject_id)),
            ("wiki_object_id".into(), json!(object_id)),
            ("wiki_predicate".into(), json!("uses")),
            ("wiki_negated".into(), json!(true)),
            ("wiki_modality".into(), json!("possible")),
            ("wiki_valid_from".into(), json!("2025-01-01")),
            ("wiki_valid_until".into(), json!("2026-01-01")),
        ]));
        let assertion = CanonicalRecord::new(fields).unwrap();
        let bytes =
            record_bytes(assertion.clone(), b"# Relationship\n\nMira uses `Cedar`.\n").unwrap();
        let note = parse_note(&bytes);
        let assertion = row(assertion, &bytes, "assertion.md");
        let expected = GraphRow { target_id: RecordId::new("assertion_use").unwrap(), target_kind: RecordKind::Assertion,
            name: String::new(), aliases: vec![], endpoints: "Mira M café Cedar C".into(), predicate: "uses".into(),
            qualifiers: "{\"wiki_modality\":\"possible\",\"wiki_negated\":true,\"wiki_valid_from\":\"2025-01-01\",\"wiki_valid_until\":\"2026-01-01\"}".into(),
            description: "Relationship\nMira uses Cedar.\n".into() };
        assert_eq!(
            graph_row(&assertion, Some(&note), [Some(&subject), Some(&object)]).unwrap(),
            expected
        );

        let entity_bytes =
            record_bytes(subject.clone(), b"# Description\n\nCurrent **text**.\n").unwrap();
        let entity_note = parse_note(&entity_bytes);
        let mut entity = row(subject, &entity_bytes, "entity.md");
        entity.identity_eligibility = Some(Eligibility::Current);
        entity.description_eligibility = Some(Eligibility::Current);
        assert_eq!(
            graph_row(&entity, Some(&entity_note), [None, None])
                .unwrap()
                .description,
            "Description\nCurrent text.\n"
        );
        entity.eligibility = Eligibility::Stale;
        entity.description_eligibility = Some(Eligibility::Stale);
        let stale = graph_row(&entity, Some(&entity_note), [None, None]).unwrap();
        assert!(stale.description.is_empty());
        assert_eq!(stale.name, "Mira");
        assert_eq!(stale.aliases, vec!["M", "café"]);
        assert_eq!(stale.qualifiers, "{}");
        assert_eq!(entity.identity_eligibility, Some(Eligibility::Current));
    }

    #[test]
    fn full_projection_title_only_edit_keeps_immutable_content_title_and_fields() {
        let temp = tempfile::tempdir().unwrap();
        let vault_id = RecordId::new("vault_row_projection").unwrap();
        fs::write(
            temp.path().join("WIKI.md"),
            record_bytes(
                CanonicalRecord::new(common(&vault_id, RecordKind::Vault, "Projection fixture"))
                    .unwrap(),
                b"",
            )
            .unwrap(),
        )
        .unwrap();
        let fs_handle = VaultFs::new(VaultRoot::explicit(temp.path()).unwrap());
        let store = SourceStore::new(fs_handle.clone());
        let content = "# Captured café\n\nByte-exact `Cedar` text.\n";
        let plan = store
            .plan_capture(CaptureRequest {
                title: "Immutable captured title".into(),
                origin_kind: SourceOrigin::LocalFile,
                origin: "fixture.md".into(),
                original: content.as_bytes().to_vec(),
                extraction: ExtractionInput::Utf8Preserve,
                media_type: Some("text/markdown".into()),
            })
            .unwrap();
        let source_id = plan.source_id.clone();
        let revision_id = plan.revision_id.clone();
        for operation in plan.draft.unwrap().operations {
            let absolute = temp.path().join(operation.target.as_str());
            fs::create_dir_all(absolute.parent().unwrap()).unwrap();
            fs::write(absolute, operation.proposed.unwrap()).unwrap();
        }
        let before = scan::scan(&fs_handle, &vault_id).unwrap();
        let source_path = path(&format!("sources/{source_id}/source.md"));
        let absolute = temp.path().join(source_path.as_str());
        let source_note = parse_note(&fs::read(&absolute).unwrap());
        fs::write(
            absolute,
            edit_note(
                &source_note,
                &BTreeMap::from([("title".into(), json!("Updated source display"))]),
                None,
                &source_note.source_hash,
            )
            .unwrap(),
        )
        .unwrap();
        let after = scan::scan(&fs_handle, &vault_id).unwrap();
        let find_capture = |projection: &super::super::CatalogProjection| {
            projection
                .documents
                .iter()
                .find(|row| row.owner_revision.as_ref() == Some(&revision_id))
                .unwrap()
                .clone()
        };
        let captured = find_capture(&after);
        assert_eq!(captured, find_capture(&before));
        assert_eq!(
            captured,
            DocumentRow {
                path: path(&format!(
                    "sources/{source_id}/revisions/{revision_id}/content.md"
                )),
                hash: Blake3Hash::digest(content),
                record_id: None,
                kind: None,
                title: "Immutable captured title".into(),
                aliases: vec![],
                headings: "Captured café".into(),
                tags: vec![],
                body: "Captured café\nByte-exact Cedar text.\n".into(),
                raw_text: content.into(),
                source_id: Some(source_id),
                owner_revision: Some(revision_id),
                eligibility: Eligibility::Current,
                reasons: vec![]
            }
        );
        assert_eq!(
            after
                .documents
                .iter()
                .find(|row| row.path == source_path)
                .unwrap()
                .title,
            "Updated source display"
        );
    }
}

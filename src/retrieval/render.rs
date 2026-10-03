//! Eligibility-aware render-v1. Synthetic headers never become evidence spans.
use super::{segment, spaces::*};
use crate::{
    catalog::{DocumentRow, ReaderSnapshot, RecordRow},
    domain::*,
    providers::types::EmbeddingInput,
    records::parse_note,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    Document,
    Entity,
    Assertion,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RenderedUnit {
    pub unit_id: Blake3Hash,
    pub owner: VaultRelativePath,
    pub target: TargetKind,
    pub target_id: Option<RecordId>,
    pub source_hash: Blake3Hash,
    pub source_span: Option<ByteSpan>,
    pub dependency_fingerprint: Blake3Hash,
    pub input_hash: Blake3Hash,
    pub utf8: String,
}
impl RenderedUnit {
    pub fn input(&self) -> EmbeddingInput {
        EmbeddingInput {
            input_hash: self.input_hash.clone(),
            utf8: self.utf8.clone(),
        }
    }
}
fn quoted(value: &impl Serialize) -> Result<String> {
    struct BoundedJson(Vec<u8>);
    impl std::io::Write for BoundedJson {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.0.len().saturating_add(bytes.len()) > 128 * 1024 {
                return Err(std::io::Error::other(
                    "embedding header field exceeds bound",
                ));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = BoundedJson(Vec::new());
    serde_json::to_writer(&mut output, value).map_err(|_| {
        WikiError::new(
            ErrorCode::BudgetExceeded,
            "embedding header field exceeds bound",
        )
    })?;
    String::from_utf8(output.0).map_err(|_| WikiError::invalid("header UTF-8 encoding"))
}
fn unit(
    document: &DocumentRow,
    target: TargetKind,
    span: Option<ByteSpan>,
    dependency_fingerprint: Blake3Hash,
    utf8: String,
) -> Result<RenderedUnit> {
    let identity = (
        &document.record_id,
        &document.path,
        &document.hash,
        span,
        SEGMENT_VERSION,
        target,
        if span.is_none() {
            Some(Blake3Hash::digest(utf8.as_bytes()))
        } else {
            None
        },
    );
    Ok(RenderedUnit {
        unit_id: Blake3Hash::digest(crate::graph::packet::canonical_json(&identity)?),
        owner: document.path.clone(),
        target,
        target_id: document
            .record_id
            .clone()
            .or_else(|| document.owner_revision.clone()),
        source_hash: document.hash.clone(),
        source_span: span,
        dependency_fingerprint,
        input_hash: Blake3Hash::digest(utf8.as_bytes()),
        utf8,
    })
}
fn dependency(
    reader: &ReaderSnapshot,
    row: Option<&RecordRow>,
    document: &DocumentRow,
) -> Result<Blake3Hash> {
    // Canonical decisions/eligibility are recomputed in fresh snapshots. Only target closure,
    // not unrelated paid run notes, binds reusable membership.
    let binding = row.map(|r| {
        (
            &r.dependencies,
            r.eligibility,
            r.identity_eligibility,
            r.description_eligibility,
        )
    });
    Ok(Blake3Hash::digest(crate::graph::packet::canonical_json(
        &(
            document.hash.clone(),
            binding,
            reader.projection().parser_fingerprint.clone(),
        ),
    )?))
}
// Parse only the bounded envelope window; do not clone the pinned full Markdown body.
fn body_start(raw: &str) -> usize {
    let mut end = raw
        .len()
        .min(crate::records::ParseLimits::default().max_envelope_bytes + 16);
    while !raw.is_char_boundary(end) {
        end -= 1;
    }
    parse_note(&raw.as_bytes()[..end]).body_start
}
pub fn corpus_iter<'a>(
    reader: &'a ReaderSnapshot,
    settings: &'a EmbeddingSettings,
) -> Result<impl Iterator<Item = Result<RenderedUnit>> + 'a> {
    settings.validate()?;
    Ok(reader
        .projection()
        .documents
        .iter()
        .flat_map(move |document| {
            let row = document
                .record_id
                .as_ref()
                .and_then(|id| reader.projection().records.get(id));
            let units: Result<Box<dyn Iterator<Item = Result<RenderedUnit>> + 'a>> = match document
                .kind
            {
                Some(RecordKind::Entity)
                    if row
                        .is_some_and(|r| r.identity_eligibility == Some(Eligibility::Current)) =>
                {
                    render_graph(reader, document, row.expect("matched"), settings)
                }
                Some(RecordKind::Assertion) if document.eligibility == Eligibility::Current => row
                    .ok_or_else(|| WikiError::invalid("assertion record missing"))
                    .and_then(|r| render_graph(reader, document, r, settings)),
                Some(RecordKind::Page) | None if document.eligibility == Eligibility::Current => {
                    render_document_iter(reader, document, settings)
                }
                _ if document.owner_revision.is_some()
                    && document.eligibility == Eligibility::Current =>
                {
                    render_document_iter(reader, document, settings)
                }
                _ => Ok(Box::new(std::iter::empty())),
            };
            match units {
                Ok(units) => units,
                Err(e) => Box::new(std::iter::once(Err(e))),
            }
        }))
}
pub fn corpus(reader: &ReaderSnapshot, settings: &EmbeddingSettings) -> Result<Vec<RenderedUnit>> {
    let mut result = corpus_iter(reader, settings)?.collect::<Result<Vec<_>>>()?;
    result.sort_by(|a, b| {
        a.target
            .cmp(&b.target)
            .then(a.target_id.cmp(&b.target_id))
            .then(a.owner.cmp(&b.owner))
            .then(a.unit_id.cmp(&b.unit_id))
    });
    Ok(result)
}
pub(crate) fn render_document_iter<'a>(
    reader: &'a ReaderSnapshot,
    document: &'a DocumentRow,
    settings: &'a EmbeddingSettings,
) -> Result<Box<dyn Iterator<Item = Result<RenderedUnit>> + 'a>> {
    let raw = &document.raw_text;
    let start = if document.owner_revision.is_some() {
        0
    } else {
        body_start(raw)
    };
    if document.title.len() > settings.max_input_bytes {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "unsplittable document title exceeds bound",
        ));
    }
    let prefix = format!(
        "{}Title: {}\n",
        settings.document_prefix,
        quoted(&document.title)?
    );
    let whole_header = format!("{prefix}Headings: []\n\n");
    let quality = settings
        .quality_target_bytes
        .unwrap_or(settings.max_input_bytes);
    let row = document
        .record_id
        .as_ref()
        .and_then(|id| reader.projection().records.get(id));
    let dep = dependency(reader, row, document)?;
    if whole_header.len() + raw.len() - start <= quality.min(settings.max_input_bytes) {
        return Ok(Box::new(std::iter::once(unit(
            document,
            TargetKind::Document,
            Some(ByteSpan::new(start as u64, raw.len() as u64)?),
            dep,
            format!("{whole_header}{}", &raw[start..]),
        ))));
    }
    // Bound every possible ancestry header before splitting; actual labels are included.
    let mut ancestry = Vec::new();
    let mut max_header = whole_header.len();
    for line in raw[start..].lines() {
        if let Some((level, label)) = segment::heading(line) {
            if label.len() > settings.max_input_bytes {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "unsplittable heading exceeds bound",
                ));
            }
            segment::push_heading(&mut ancestry, level, label);
            max_header = max_header.max(
                prefix.len() + "Headings: \n\n".len() + quoted(&segment::labels(&ancestry))?.len(),
            );
        }
    }
    Ok(Box::new(
        segment::split_iter(
            raw,
            start,
            max_header,
            settings.max_input_bytes,
            settings.quality_target_bytes,
        )?
        .map(move |slice| {
            let slice = slice?;
            let utf8 = format!(
                "{prefix}Headings: {}\n\n{}",
                quoted(&slice.headings)?,
                slice.text
            );
            if utf8.len() > quality.min(settings.max_input_bytes) {
                return Err(WikiError::new(
                    ErrorCode::BudgetExceeded,
                    "formatted document embedding exceeds input bound",
                ));
            }
            unit(
                document,
                TargetKind::Document,
                Some(slice.span),
                dep.clone(),
                utf8,
            )
        }),
    ))
}
pub fn render_document(
    reader: &ReaderSnapshot,
    document: &DocumentRow,
    settings: &EmbeddingSettings,
) -> Result<Vec<RenderedUnit>> {
    render_document_iter(reader, document, settings)?.collect()
}
fn endpoint(reader: &ReaderSnapshot, id: &str) -> Result<Value> {
    let id = RecordId::new(id)?;
    let row = reader
        .projection()
        .records
        .get(&id)
        .filter(|r| {
            r.record.kind() == RecordKind::Entity
                && r.identity_eligibility == Some(Eligibility::Current)
        })
        .ok_or_else(|| {
            WikiError::new(
                ErrorCode::FreshnessConflict,
                "assertion endpoint identity unavailable",
            )
        })?;
    Ok(
        json!({"label":row.record.title(),"type":row.record.string("wiki_entity_type").unwrap_or("")}),
    )
}
pub fn render_graph<'a>(
    reader: &'a ReaderSnapshot,
    document: &'a DocumentRow,
    row: &'a RecordRow,
    settings: &'a EmbeddingSettings,
) -> Result<Box<dyn Iterator<Item = Result<RenderedUnit>> + 'a>> {
    let r = &row.record;
    if r.title().len() > settings.max_input_bytes
        || r.field("aliases")
            .and_then(Value::as_array)
            .is_some_and(|a| a.len() > settings.max_input_bytes)
    {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "unsplittable graph identity exceeds bound",
        ));
    }
    let mut header = settings.document_prefix.clone();
    let target = if r.kind() == RecordKind::Entity {
        let mut aliases = r
            .field("aliases")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>())
            .unwrap_or_default();
        if aliases.len() > settings.max_input_bytes
            || aliases
                .iter()
                .try_fold(0usize, |sum, alias| sum.checked_add(alias.len()))
                .is_none_or(|bytes| bytes > settings.max_input_bytes)
        {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "unsplittable aliases exceed bound",
            ));
        }
        aliases.sort();
        aliases.dedup();
        header.push_str(&format!(
            "Entity: {}\nType: {}\nAliases: {}\n",
            quoted(&r.title())?,
            quoted(&r.string("wiki_entity_type").unwrap_or(""))?,
            quoted(&aliases)?
        ));
        TargetKind::Entity
    } else {
        // Endpoint objects require fixed label/type key order, independently of JSON map ordering.
        let render_endpoint = |value: Value| -> Result<String> {
            Ok(format!(
                "{{\"label\":{},\"type\":{}}}",
                quoted(&value["label"])?,
                quoted(&value["type"])?
            ))
        };
        header.push_str(&format!(
            "Subject: {}\nPredicate: {}\n",
            render_endpoint(endpoint(
                reader,
                r.string("wiki_subject_id")
                    .ok_or_else(|| WikiError::invalid("subject missing"))?
            )?)?,
            quoted(&r.string("wiki_predicate").unwrap_or(""))?
        ));
        let object = if let Some(id) = r.string("wiki_object_id") {
            render_endpoint(endpoint(reader, id)?)?
        } else {
            format!(
                "{{\"type\":{},\"value\":{}}}",
                quoted(&r.string("wiki_literal_type").unwrap_or(""))?,
                quoted(&r.string("wiki_literal_value").unwrap_or(""))?
            )
        };
        header.push_str(&format!(
            "Object: {object}\nNegated: {}\nModality: {}\n",
            r.field("wiki_negated")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            quoted(&r.string("wiki_modality").unwrap_or("asserted"))?
        ));
        for (label, field) in [
            ("ValidFrom", "wiki_valid_from"),
            ("ValidUntil", "wiki_valid_until"),
            ("Property", "wiki_property"),
            ("Unit", "wiki_unit"),
        ] {
            if let Some(value) = r.field(field) {
                header.push_str(&format!("{label}: {}\n", quoted(value)?));
            }
        }
        TargetKind::Assertion
    };
    let body = &document.raw_text[body_start(&document.raw_text)..];
    let description = r.string("description").unwrap_or(body);
    let permitted = target == TargetKind::Assertion && row.eligibility == Eligibility::Current
        || target == TargetKind::Entity
            && row.description_eligibility == Some(Eligibility::Current);
    let description = if permitted { description } else { "" };
    // Establish original source mapping once, avoiding a whole-note scan per segment.
    let source_offset = if description == body {
        Some(body_start(&document.raw_text))
    } else {
        let mut matches = document.raw_text.match_indices(description);
        matches
            .next()
            .map(|(offset, _)| offset)
            .filter(|_| matches.next().is_none())
    };
    // Graph descriptions are eligible readable text; raw backing Markdown may include
    // duplicate evidence quotations, so never embed the entire assertion/evidence note.
    let dep = dependency(reader, Some(row), document)?;
    if description.is_empty() {
        if header.len()
            > settings
                .quality_target_bytes
                .unwrap_or(settings.max_input_bytes)
                .min(settings.max_input_bytes)
        {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "unsplittable graph header exceeds bound",
            ));
        }
        return Ok(Box::new(std::iter::once(unit(
            document, target, None, dep, header,
        ))));
    }
    header.push_str("Description: ");
    let bound = settings
        .quality_target_bytes
        .unwrap_or(settings.max_input_bytes)
        .min(settings.max_input_bytes);
    if header.len() >= bound {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "unsplittable graph header exceeds bound",
        ));
    }
    Ok(Box::new(
        segment::split_iter(
            description,
            0,
            header.len(),
            settings.max_input_bytes,
            settings.quality_target_bytes,
        )?
        .map(move |slice| {
            let slice = slice?;
            // Description may originate in frontmatter or original Markdown.
            // Mapping to raw body is only claimed for an exact unique byte match.
            let span = source_offset
                .map(|offset| {
                    ByteSpan::new(
                        offset as u64 + slice.span.start(),
                        offset as u64 + slice.span.end(),
                    )
                })
                .transpose()?;
            let mut rendered = unit(
                document,
                target,
                span,
                dep.clone(),
                format!("{header}{}", slice.text),
            )?;
            // Logical segment identity disambiguates repeated descriptions without fabricating citations.
            if span.is_none() {
                rendered.unit_id = Blake3Hash::digest(crate::graph::packet::canonical_json(&(
                    rendered.unit_id,
                    slice.span,
                ))?);
            }
            Ok(rendered)
        }),
    ))
}

//! Conservative field-value splicing. No whole-envelope serialization.
use super::parse::{ParsedNote, line, parse_note};
use crate::domain::{Blake3Hash, CanonicalRecord, ErrorCode, RecordKind, Result, WikiError};
use serde_json::Value;
use std::{collections::BTreeMap, ops::Range};

/// Prepare bytes only. The caller must apply through the recoverable changeset engine.
pub fn edit_note(
    note: &ParsedNote,
    changes: &BTreeMap<String, Value>,
    body: Option<&[u8]>,
    expected_hash: &Blake3Hash,
) -> Result<Vec<u8>> {
    if &note.source_hash != expected_hash || &Blake3Hash::digest(&note.raw) != expected_hash {
        return Err(WikiError::new(
            ErrorCode::ContentConflict,
            "note hash differs from expected hash",
        ));
    }
    let record = note
        .canonical
        .as_ref()
        .filter(|_| note.is_editable())
        .ok_or_else(|| WikiError::invalid("structured edits require valid supported metadata"))?;
    if matches!(
        record.kind(),
        RecordKind::Revision | RecordKind::ExtractionPacket | RecordKind::RunEvent
    ) {
        return Err(WikiError::invalid("immutable records require a successor"));
    }
    if let Some(body) = body {
        std::str::from_utf8(body)
            .map_err(|_| WikiError::invalid("replacement body must be UTF-8"))?;
    }
    let mut fields = record.fields().clone();
    for (key, value) in changes {
        if matches!(key.as_str(), "wiki_id" | "wiki_kind" | "wiki_schema") {
            return Err(WikiError::invalid(
                "identity, kind and schema require a separate migration operation",
            ));
        }
        if !(key.starts_with("wiki_")
            || matches!(key.as_str(), "title" | "description" | "aliases" | "tags"))
        {
            return Err(WikiError::invalid("only known properties may be edited"));
        }
        fields.insert(key.clone(), value.clone());
    }
    CanonicalRecord::new(fields)?;
    let mut splices: Vec<(Range<usize>, Vec<u8>)> = vec![];
    let mut appended = Vec::new();
    for (key, value) in changes {
        if record.field(key) == Some(value) {
            continue;
        }
        let serialized =
            serde_json::to_vec(value).map_err(|e| WikiError::invalid(e.to_string()))?;
        if let Some(start) = note.field_starts.get(key) {
            let range = editable_range(note, *start, key)?;
            splices.push((range, serialized));
        } else {
            appended.extend_from_slice(key.as_bytes());
            appended.extend_from_slice(b": ");
            appended.extend(serialized);
            appended.extend_from_slice(note.newline.as_bytes());
        }
    }
    if !appended.is_empty() {
        splices.push((note.closing_start..note.closing_start, appended));
    }
    if let Some(body) = body {
        splices.push((note.body_start..note.raw.len(), body.to_vec()));
    }
    splices.sort_by_key(|(range, _)| range.start);
    let mut output = Vec::new();
    let mut cursor = 0;
    for (range, bytes) in splices {
        if range.start < cursor {
            return Err(WikiError::invalid("overlapping edit ranges"));
        }
        output.extend_from_slice(&note.raw[cursor..range.start]);
        output.extend(bytes);
        cursor = range.end;
    }
    output.extend_from_slice(&note.raw[cursor..]);
    let reparsed = parse_note(&output);
    if !reparsed.is_editable() {
        return Err(WikiError::invalid(
            "requested edit cannot safely preserve the envelope",
        ));
    }
    Ok(output)
}
fn editable_range(note: &ParsedNote, key_start: usize, key: &str) -> Result<Range<usize>> {
    let fail = || {
        WikiError::invalid(format!(
            "cannot safely edit multiline or complex field {key:?}"
        ))
    };
    let line_start = note.raw[..key_start]
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(0, |i| i + 1);
    // A flow root, explicit complex key, or multiple properties on one line has
    // no supported range. Top-level indentation is allowed and retained.
    if note.raw[line_start..key_start]
        .iter()
        .any(|b| !b.is_ascii_whitespace())
    {
        return Err(fail());
    }
    let (content, _, _) = line(&note.raw, key_start);
    let mut quote = None;
    let mut escape = false;
    let mut colon = None;
    for (i, &b) in content.iter().enumerate() {
        if escape {
            escape = false;
            continue;
        }
        if quote == Some(b'"') && b == b'\\' {
            escape = true;
            continue;
        }
        if matches!(b, b'\'' | b'"') {
            if quote == Some(b) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(b);
            }
            continue;
        }
        if quote.is_none() && b == b':' {
            colon = Some(i);
            break;
        }
    }
    let mut start = colon.ok_or_else(fail)? + 1;
    while start < content.len() && content[start].is_ascii_whitespace() {
        start += 1;
    }
    if start == content.len() || matches!(content[start], b'|' | b'>') {
        return Err(fail());
    }
    quote = None;
    escape = false;
    let mut depth = 0usize;
    let mut end = content.len();
    for i in start..content.len() {
        let b = content[i];
        if escape {
            escape = false;
            continue;
        }
        if quote == Some(b'"') && b == b'\\' {
            escape = true;
            continue;
        }
        if matches!(b, b'\'' | b'"') {
            if quote == Some(b) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(b);
            }
            continue;
        }
        if quote.is_none() {
            match b {
                b'[' | b'{' => depth += 1,
                b']' | b'}' => depth = depth.checked_sub(1).ok_or_else(fail)?,
                b'#' if depth == 0 && (i == start || content[i - 1].is_ascii_whitespace()) => {
                    end = i;
                    break;
                }
                _ => {}
            }
        }
    }
    if quote.is_some() || depth != 0 {
        return Err(fail());
    }
    while end > start && content[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    // Refuse continuation lines, block collections and multiline plain scalars.
    let next = note
        .field_starts
        .values()
        .copied()
        .filter(|offset| *offset > key_start)
        .min()
        .unwrap_or(note.closing_start);
    let line_end = line(&note.raw, key_start).1;
    let rest = std::str::from_utf8(&note.raw[line_end..next]).map_err(|_| fail())?;
    if rest.lines().any(|line| {
        let s = line.trim();
        !s.is_empty() && !s.starts_with('#')
    }) {
        return Err(fail());
    }
    if end <= start {
        return Err(fail());
    }
    Ok(key_start + start..key_start + end)
}

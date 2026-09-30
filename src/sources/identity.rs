//! Conservative identity reservation for malformed canonical envelopes.
use crate::{
    domain::RecordId,
    records::{ParsedNote, parse_note},
};
use std::collections::{BTreeMap, BTreeSet};

/// Readable declarations reserve IDs even when another field prevents adoption.
pub(crate) fn readable_ids(note: &ParsedNote) -> BTreeSet<RecordId> {
    if let Some(value) = note
        .fields
        .as_ref()
        .and_then(|fields| fields.get("wiki_id"))
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

/// Strictly parse independent top-level fields only as a reservation aid.
/// These fields never create adopted records.
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

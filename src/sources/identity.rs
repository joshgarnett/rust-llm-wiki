//! Bounded, conservative identity reservation for canonical envelopes.
use crate::{
    domain::{ErrorCode, RecordId, Result, WikiError},
    records::{ParsedNote, parse_note},
};
use std::collections::{BTreeMap, BTreeSet};

/// Count raw envelope bytes, including line endings, but not delimiter lines.
pub(crate) use crate::records::envelope_observation::{
    MAX_OBSERVATION_BYTES as MAX_RESERVATION_BYTES,
    MAX_OBSERVATION_FIELDS as MAX_RESERVATION_FIELDS,
};

fn incomplete() -> WikiError {
    WikiError::new(
        ErrorCode::BudgetExceeded,
        "wiki envelope identity observation incomplete: exceeds 256 KiB or 4096 top-level candidates",
    )
}

struct EnvelopeClaims {
    fields: Vec<BTreeMap<String, serde_json::Value>>,
    wiki_claim: bool,
}

/// A bounded scan must complete before any claims or absence can be used.
fn scan(note: &ParsedNote) -> Result<EnvelopeClaims> {
    let raw = note.raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&note.raw);
    let opening = if raw.starts_with(b"---\r\n") {
        5
    } else if raw.starts_with(b"---\n") {
        4
    } else if raw == b"---" {
        3
    } else {
        0
    };
    let mut result = EnvelopeClaims {
        fields: vec![],
        wiki_claim: false,
    };
    let mut remaining = if opening == 0 {
        let Some(region) = crate::records::envelope_observation::suspected_region(&note.raw)?
        else {
            return Ok(result);
        };
        result.wiki_claim = true;
        &note.raw[region]
    } else {
        &raw[opening..]
    };
    let mut bytes = 0;
    let mut candidates = Vec::new();
    let mut candidate = Vec::new();
    while !remaining.is_empty() {
        // Inspect only the remaining allowance and room for a closing delimiter.
        // A huge single line cannot force a scan of the rest of the file.
        let bounded = &remaining[..remaining.len().min(MAX_RESERVATION_BYTES - bytes + 5)];
        let length = bounded
            .iter()
            .position(|b| *b == b'\n')
            .map_or(bounded.len(), |n| n + 1);
        let line = &bounded[..length];
        let content = line.strip_suffix(b"\n").unwrap_or(line);
        let content = content.strip_suffix(b"\r").unwrap_or(content);
        if content == b"---" {
            break;
        }
        bytes += length;
        if bytes > MAX_RESERVATION_BYTES
            || (length == bounded.len() && length < remaining.len() && !line.ends_with(b"\n"))
        {
            return Err(incomplete());
        }
        if !content.is_empty() && !content.starts_with(b" ") && !content.starts_with(b"\t") {
            if !candidate.is_empty() {
                candidates.push(std::mem::take(&mut candidate));
            }
            if candidates.len() >= MAX_RESERVATION_FIELDS {
                return Err(incomplete());
            }
            let key = content.split(|b| *b == b':').next().unwrap_or_default();
            let key = key.trim_ascii();
            let key = key
                .strip_prefix(b"\"")
                .and_then(|v| v.strip_suffix(b"\""))
                .or_else(|| key.strip_prefix(b"'").and_then(|v| v.strip_suffix(b"'")))
                .unwrap_or(key);
            result.wiki_claim |= key.starts_with(b"wiki_");
        }
        candidate.extend_from_slice(line);
        remaining = &remaining[length..];
    }
    if !candidate.is_empty() {
        candidates.push(candidate);
    }
    if candidates.len() > MAX_RESERVATION_FIELDS {
        return Err(incomplete());
    }
    if let Some(fields) = &note.fields {
        result.wiki_claim |= fields.keys().any(|key| key.starts_with("wiki_"));
        result.fields.push(fields.clone());
    } else {
        for candidate in candidates {
            let mut bytes = b"---\n".to_vec();
            bytes.extend(candidate);
            if !bytes.ends_with(b"\n") {
                bytes.push(b'\n');
            }
            bytes.extend_from_slice(b"---\n");
            if let Some(fields) = parse_note(&bytes).fields {
                result.wiki_claim |= fields.keys().any(|key| key.starts_with("wiki_"));
                result.fields.push(fields);
            }
        }
    }
    Ok(result)
}

/// Readable declarations reserve every distinct ID, even in malformed envelopes.
pub(crate) fn readable_ids(note: &ParsedNote) -> Result<BTreeSet<RecordId>> {
    Ok(isolated_fields(note)?
        .iter()
        .filter_map(|fields| fields.get("wiki_id"))
        .filter_map(serde_json::Value::as_str)
        .filter_map(|value| RecordId::new(value).ok())
        .collect())
}

/// A positive declaration is a boundary, never authority to adopt or repair.
pub(crate) fn has_wiki_claim(note: &ParsedNote) -> Result<bool> {
    Ok(scan(note)?.wiki_claim || note.canonical.is_some())
}

pub(crate) fn isolated_fields(
    note: &ParsedNote,
) -> Result<Vec<BTreeMap<String, serde_json::Value>>> {
    Ok(scan(note)?.fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_distinct_malformed_claims_are_reserved() {
        let note = parse_note(b"\xef\xbb\xbf---\r\nwiki_id: first\r\nwiki_id: second\r\nwiki_id: first\r\nbroken: [\r\n");
        assert_eq!(
            readable_ids(&note).unwrap(),
            BTreeSet::from([
                RecordId::new("first").unwrap(),
                RecordId::new("second").unwrap()
            ])
        );
        assert!(has_wiki_claim(&note).unwrap());
    }

    #[test]
    fn byte_limit_never_returns_partial_claims() {
        for closed in [false, true] {
            let mut bytes = b"---\nwiki_id: before\n#".to_vec();
            bytes.resize(4 + MAX_RESERVATION_BYTES - 1, b'x');
            bytes.push(b'\n');
            if closed {
                bytes.extend_from_slice(b"---\nbody");
            }
            assert!(
                readable_ids(&parse_note(&bytes))
                    .unwrap()
                    .contains(&RecordId::new("before").unwrap())
            );
            let at = 4 + MAX_RESERVATION_BYTES;
            bytes.splice(at..at, b"wiki_id: after\n".iter().copied());
            assert_eq!(
                readable_ids(&parse_note(&bytes)).unwrap_err().code,
                ErrorCode::BudgetExceeded
            );
        }
        let mut huge = b"---\n".to_vec();
        huge.resize(16 * 1024 * 1024, b'x');
        assert_eq!(
            has_wiki_claim(&parse_note(&huge)).unwrap_err().code,
            ErrorCode::BudgetExceeded
        );
    }

    #[test]
    fn field_limit_preserves_all_ids_or_fails() {
        let mut bytes = b"---\n".to_vec();
        for n in 0..MAX_RESERVATION_FIELDS {
            bytes.extend_from_slice(format!("wiki_id: id_{n}\n").as_bytes());
        }
        assert_eq!(
            readable_ids(&parse_note(&bytes)).unwrap().len(),
            MAX_RESERVATION_FIELDS
        );
        bytes.extend_from_slice(b"wiki_id: after\n---\n");
        assert_eq!(
            readable_ids(&parse_note(&bytes)).unwrap_err().code,
            ErrorCode::BudgetExceeded
        );
    }

    #[test]
    fn suspected_opener_reserves_all_string_ids_only_inside_its_leading_region() {
        for opener in ["", "--\n", "----\n", "\u{feff}--\r\n"] {
            let text = format!(
                "{opener}wiki_schema: [\nwiki_kind: page\nwiki_id: first\nwiki_id: second\nwiki_id: 123\nwiki_id: '123'\ndescription: |\n  wiki_id: nested\n\nwiki_id: body\n"
            );
            let note = parse_note(text.as_bytes());
            assert!(note.canonical.is_none());
            assert!(note.fields.is_none());
            assert!(has_wiki_claim(&note).unwrap());
            assert_eq!(
                readable_ids(&note).unwrap(),
                ["first", "second", "123"]
                    .into_iter()
                    .map(|value| RecordId::new(value).unwrap())
                    .collect()
            );
        }
        let missing_id = parse_note(b"--\nwiki_schema: '1'\nwiki_kind: page\n");
        assert!(has_wiki_claim(&missing_id).unwrap());
        assert!(readable_ids(&missing_id).unwrap().is_empty());
        for text in [
            "wiki_id: first\nwiki_id: second\n",
            "title: |\n  wiki_schema: '1'\n  wiki_kind: page\n  wiki_id: nested\n",
            "# Example\nwiki_schema: '1'\nwiki_kind: page\nwiki_id: example\n",
        ] {
            assert!(!has_wiki_claim(&parse_note(text.as_bytes())).unwrap());
            assert!(
                readable_ids(&parse_note(text.as_bytes()))
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[test]
    fn suspected_region_limits_never_reserve_a_partial_set() {
        let mut text = b"--\nwiki_schema: '1'\nwiki_id: before\n".to_vec();
        for n in 2..MAX_RESERVATION_FIELDS {
            text.extend_from_slice(format!("wiki_id: id_{n}\n").as_bytes());
        }
        assert_eq!(
            readable_ids(&parse_note(&text)).unwrap().len(),
            MAX_RESERVATION_FIELDS - 1
        );
        text.extend_from_slice(b"wiki_id: beyond\n");
        assert_eq!(
            readable_ids(&parse_note(&text)).unwrap_err().code,
            ErrorCode::BudgetExceeded
        );
        let mut text = b"wiki_schema: '1'\nwiki_id: before\nauthor: ".to_vec();
        text.resize(MAX_RESERVATION_BYTES - 1, b'x');
        text.push(b'\n');
        assert!(
            readable_ids(&parse_note(&text))
                .unwrap()
                .contains(&RecordId::new("before").unwrap())
        );
        text.extend_from_slice(b"wiki_id: beyond\n");
        assert_eq!(
            readable_ids(&parse_note(&text)).unwrap_err().code,
            ErrorCode::BudgetExceeded
        );
    }

    #[test]
    fn ordinary_yaml_and_code_examples_make_no_claim() {
        for raw in [
            b"---\nauthor: me\n---\nwiki_id: body".as_slice(),
            b"# Note\nwiki_id: body",
            b"---\nunknown: [\n---\nbody",
        ] {
            assert!(!has_wiki_claim(&parse_note(raw)).unwrap());
        }
        assert!(has_wiki_claim(&parse_note(b"---\n'wiki_id': [\n---\n")).unwrap());
    }
}

//! Source-only suspicion of a damaged leading wiki envelope, never adoption.
use crate::domain::{ErrorCode, Result, WikiError};
use std::ops::Range;

pub(crate) const MAX_OBSERVATION_BYTES: usize = 256 * 1024;
pub(crate) const MAX_OBSERVATION_FIELDS: usize = 4096;

fn incomplete() -> WikiError {
    let mut error = WikiError::new(
        ErrorCode::BudgetExceeded,
        "leading envelope observation incomplete: exceeds 256 KiB or 4096 top-level candidates",
    );
    error.details = serde_json::json!({"reason": "envelope_observation_incomplete"});
    error
}

struct Line<'a> {
    content: &'a [u8],
    bytes: usize,
    complete: bool,
}
fn bounded_line(raw: &[u8], allowance: usize) -> Line<'_> {
    let sample = &raw[..raw.len().min(allowance.saturating_add(5))];
    let bytes = sample
        .iter()
        .position(|b| *b == b'\n')
        .map_or(sample.len(), |n| n + 1);
    let line = &sample[..bytes];
    let content = if let Some(content) = line.strip_suffix(b"\n") {
        content.strip_suffix(b"\r").unwrap_or(content)
    } else {
        line
    };
    Line {
        content,
        bytes,
        complete: bytes == raw.len() || line.ends_with(b"\n"),
    }
}
fn horizontal(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t')
}
fn key_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

/// Deliberately smaller than YAML. Values may be malformed; keys stay explicit.
fn declaration_key(line: &[u8]) -> Option<&[u8]> {
    if line.first().is_none_or(|b| horizontal(*b)) {
        return None;
    }
    let colon = line.iter().position(|b| *b == b':')?;
    if line.get(colon + 1).is_some_and(|b| !horizontal(*b)) {
        return None;
    }
    let mut key = &line[..colon];
    while key.last().is_some_and(|b| horizontal(*b)) {
        key = &key[..key.len() - 1];
    }
    if let Some(quote @ (b'\'' | b'"')) = key.first().copied() {
        key = key.strip_prefix(&[quote])?.strip_suffix(&[quote])?;
    }
    (!key.is_empty() && key.iter().all(|b| key_byte(*b))).then_some(key)
}

/// A bounded prefix may end in a still-possible long key. It cannot prove absence.
fn possible_declaration_prefix(line: &[u8]) -> bool {
    let mut index = 0;
    let quote = line.first().copied().filter(|b| matches!(b, b'\'' | b'"'));
    if quote.is_some() {
        index += 1;
    }
    let start = index;
    while line.get(index).is_some_and(|b| key_byte(*b)) {
        index += 1;
    }
    if index == start {
        return false;
    }
    if index == line.len() {
        return true;
    }
    if let Some(quote) = quote {
        if line[index] != quote {
            return false;
        }
        index += 1;
    }
    while line.get(index).is_some_and(|b| horizontal(*b)) {
        index += 1;
    }
    index == line.len()
        || (line[index] == b':' && (index + 1 == line.len() || horizontal(line[index + 1])))
}

/// Observe only byte-zero (after an optional BOM), and return the exact leading
/// mapping region only after two distinct schema/kind/id declarations. A Result
/// error is incompleteness, including before the second core key is encountered.
/// The range is reservation input, not an authenticated envelope/body boundary.
pub(crate) fn suspected_region(raw: &[u8]) -> Result<Option<Range<usize>>> {
    let bom = usize::from(raw.starts_with(b"\xef\xbb\xbf")) * 3;
    let first = bounded_line(&raw[bom..], MAX_OBSERVATION_BYTES);
    if first.complete && first.content == b"---" {
        return Ok(None);
    }
    let damaged_opener = first.complete
        && (1..=8).contains(&first.content.len())
        && first.content.iter().all(|b| *b == b'-');
    let start = bom + if damaged_opener { first.bytes } else { 0 };
    let mut position = start;
    let mut candidates = 0;
    let mut core = [false; 3];
    while position < raw.len() {
        let used = position - start;
        let line = bounded_line(&raw[position..], MAX_OBSERVATION_BYTES - used);
        if line.complete && line.content.iter().all(|b| horizontal(*b)) {
            break;
        }
        if line.complete && line.content == b"---" {
            break;
        }
        let continuation = line.content.first().is_some_and(|b| horizontal(*b));
        if continuation {
            if candidates == 0 {
                break;
            }
        } else if let Some(key) = declaration_key(line.content) {
            candidates += 1;
            match key {
                b"wiki_schema" => core[0] = true,
                b"wiki_kind" => core[1] = true,
                b"wiki_id" => core[2] = true,
                _ => {}
            }
        } else {
            if !line.complete && possible_declaration_prefix(line.content) {
                return Err(incomplete());
            }
            break;
        }
        if !line.complete
            || used + line.bytes > MAX_OBSERVATION_BYTES
            || candidates > MAX_OBSERVATION_FIELDS
        {
            return Err(incomplete());
        }
        position += line.bytes;
    }
    Ok((core.into_iter().filter(|present| *present).count() >= 2).then_some(start..position))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suspicion_requires_two_distinct_core_keys_at_the_leading_region() {
        for opening in ["", "-\n", "--\n", "----\n", "--------\n", "\u{feff}--\r\n"] {
            let text = format!(
                "{opening}wiki_schema: '1'\r\n'wiki_kind' : page\r\nwiki_id: readable\r\n---\r\nbody\r\n"
            );
            let region = suspected_region(text.as_bytes()).unwrap().unwrap();
            assert_eq!(
                &text.as_bytes()[region],
                b"wiki_schema: '1'\r\n'wiki_kind' : page\r\nwiki_id: readable\r\n"
            );
        }
        for text in [
            "---\nwiki_schema: '1'\nwiki_kind: page\n---\n",
            "---------\nwiki_schema: '1'\nwiki_kind: page\n",
            "# Example\nwiki_schema: '1'\nwiki_kind: page\n",
            "```yaml\nwiki_schema: '1'\nwiki_kind: page\n```\n",
            "\nwiki_schema: '1'\nwiki_kind: page\n",
            "author: one\ntitle: Two\n",
            "wiki_id: one\nwiki_id: two\n",
            "wiki_custom: one\nwiki_other: two\n",
            "title: |\n  wiki_schema: '1'\n  wiki_kind: page\n",
            "wiki_schema: '1'\n# comment\nwiki_kind: page\n",
            "wiki_schema:'1'\nwiki_kind:page\n",
            "wiki_schema: '1'\nhttps://example.invalid\nwiki_kind: page\n",
        ] {
            assert!(
                suspected_region(text.as_bytes()).unwrap().is_none(),
                "{text:?}"
            );
        }
    }

    #[test]
    fn observation_never_consumes_body_or_indented_core_declarations() {
        for boundary in [
            "\n",
            " \t\n",
            "# Heading\n",
            "#comment\n",
            "```yaml\n",
            "prose\n",
            "---\n",
        ] {
            let header = "wiki_schema: [\nwiki_kind: 12\n";
            let raw = format!("{header}{boundary}wiki_id: body_id\n");
            assert_eq!(
                suspected_region(raw.as_bytes()).unwrap(),
                Some(0..header.len())
            );
        }
        let text = b"wiki_schema: '1'\ndescription: |\n  wiki_kind: page\n";
        assert!(suspected_region(text).unwrap().is_none());
    }

    #[test]
    fn all_caps_fail_closed_before_or_after_suspicion() {
        for header in ["author: ", "wiki_schema: '1'\nwiki_kind: page\nauthor: "] {
            let mut raw = header.as_bytes().to_vec();
            raw.resize(MAX_OBSERVATION_BYTES - 1, b'x');
            raw.push(b'\n');
            assert!(suspected_region(&raw).is_ok());
            raw.extend_from_slice(b"wiki_id: beyond\n");
            assert_eq!(
                suspected_region(&raw).unwrap_err().code,
                ErrorCode::BudgetExceeded
            );
        }
        let mut fields = "author: one\n".repeat(MAX_OBSERVATION_FIELDS).into_bytes();
        assert_eq!(suspected_region(&fields).unwrap(), None);
        fields.extend_from_slice(b"wiki_schema: '1'\nwiki_kind: page\n");
        assert_eq!(
            suspected_region(&fields).unwrap_err().code,
            ErrorCode::BudgetExceeded
        );
        let mut huge = b"author: ".to_vec();
        huge.resize(16 * 1024 * 1024, b'x');
        assert_eq!(
            suspected_region(&huge).unwrap_err().code,
            ErrorCode::BudgetExceeded
        );
        huge[..9].copy_from_slice(b"# Heading");
        assert_eq!(suspected_region(&huge).unwrap(), None);
    }
}

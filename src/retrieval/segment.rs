//! Deterministic UTF-8-safe slices with original byte spans and no overlap.
use crate::domain::*;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodySegment {
    pub span: ByteSpan,
    pub text: String,
    pub headings: Vec<String>,
}

pub fn split_iter(
    raw: &str,
    start: usize,
    header_bytes: usize,
    max_bytes: usize,
    quality: Option<usize>,
) -> Result<impl Iterator<Item = Result<BodySegment>> + '_> {
    if start > raw.len()
        || !raw.is_char_boundary(start)
        || max_bytes == 0
        || header_bytes >= max_bytes
    {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "embedding header leaves no bounded body capacity",
        ));
    }
    let quality_limit = quality.unwrap_or(max_bytes).min(max_bytes);
    if quality_limit <= header_bytes {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "embedding header exceeds quality bound",
        ));
    }
    let capacity = quality_limit - header_bytes;
    let mut cursor = start;
    let mut empty_pending = start == raw.len();
    let mut headings: Vec<(usize, String)> = Vec::new();
    // Prefer heading then paragraph boundaries within each capacity. Preserve raw bytes.
    Ok(std::iter::from_fn(move || {
        if cursor == raw.len() {
            if !empty_pending {
                return None;
            }
            empty_pending = false;
            return Some(
                ByteSpan::new(start as u64, start as u64).map(|span| BodySegment {
                    span,
                    text: String::new(),
                    headings: labels(&headings),
                }),
            );
        }
        let mut ceiling = (cursor + capacity).min(raw.len());
        while ceiling > cursor && !raw.is_char_boundary(ceiling) {
            ceiling -= 1;
        }
        if ceiling == cursor {
            cursor = raw.len();
            return Some(Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "input capacity cannot fit one UTF-8 scalar",
            )));
        }
        let mut heading_boundary = None;
        let mut paragraph_boundary = None;
        let mut offset = cursor;
        for line in raw[cursor..ceiling].split_inclusive('\n') {
            let clean = line.trim_end_matches(['\r', '\n']);
            if offset > cursor && heading_at(raw, offset).is_some() {
                heading_boundary = Some(offset);
            }
            if clean.is_empty() && offset + line.len() > cursor {
                paragraph_boundary = Some(offset + line.len());
            }
            offset += line.len();
        }
        let end = if ceiling == raw.len() {
            ceiling
        } else {
            heading_boundary
                .or(paragraph_boundary)
                .filter(|n| *n > cursor)
                .unwrap_or(ceiling)
        };
        // Context at the segment's first line; heading ancestry is presentation only.
        if let Some((level, label)) = heading_at(raw, cursor) {
            push_heading(&mut headings, level, label);
        }
        let result = ByteSpan::new(cursor as u64, end as u64).map(|span| BodySegment {
            span,
            text: raw[cursor..end].into(),
            headings: labels(&headings),
        });
        let mut offset = cursor;
        for line in raw[cursor..end].split_inclusive('\n') {
            if offset > cursor
                && let Some((level, label)) = heading_at(raw, offset)
            {
                push_heading(&mut headings, level, label);
            }
            offset += line.len();
        }
        cursor = end;
        Some(result)
    }))
}
pub fn split(
    raw: &str,
    start: usize,
    header_bytes: usize,
    max_bytes: usize,
    quality: Option<usize>,
) -> Result<Vec<BodySegment>> {
    split_iter(raw, start, header_bytes, max_bytes, quality)?.collect()
}

pub(crate) fn heading(line: &str) -> Option<(usize, &str)> {
    let count = line.bytes().take_while(|b| *b == b'#').count();
    if count == 0 || count > 6 || !line[count..].starts_with(' ') {
        return None;
    }
    Some((count, line[count..].trim().trim_end_matches('#').trim()))
}

// A capacity split is not a Markdown line boundary. Parse the full original
// line so a split inside a heading cannot invent a different ancestry label.
fn heading_at(raw: &str, offset: usize) -> Option<(usize, &str)> {
    if offset != 0 && raw.as_bytes().get(offset - 1) != Some(&b'\n') {
        return None;
    }
    raw.get(offset..)?.lines().next().and_then(heading)
}
pub(crate) fn push_heading(headings: &mut Vec<(usize, String)>, level: usize, label: &str) {
    while headings
        .last()
        .is_some_and(|(previous, _)| *previous >= level)
    {
        headings.pop();
    }
    headings.push((level, label.to_owned()));
}
pub(crate) fn labels(headings: &[(usize, String)]) -> Vec<String> {
    headings.iter().map(|(_, label)| label.clone()).collect()
}

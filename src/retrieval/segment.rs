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
    let mut headings: Vec<String> = Vec::new();
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
                    headings: headings.clone(),
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
            if offset > cursor && heading(clean).is_some() {
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
        if let Some((level, label)) = raw[cursor..end].lines().next().and_then(heading) {
            headings.truncate(level.saturating_sub(1));
            headings.push(label);
        }
        let result = ByteSpan::new(cursor as u64, end as u64).map(|span| BodySegment {
            span,
            text: raw[cursor..end].into(),
            headings: headings.clone(),
        });
        for line in raw[cursor..end].lines().skip(1) {
            if let Some((level, label)) = heading(line) {
                headings.truncate(level.saturating_sub(1));
                headings.push(label);
            }
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

fn heading(line: &str) -> Option<(usize, String)> {
    let count = line.bytes().take_while(|b| *b == b'#').count();
    if count == 0 || count > 6 || !line[count..].starts_with(' ') {
        return None;
    }
    Some((
        count,
        line[count..].trim().trim_end_matches('#').trim().into(),
    ))
}

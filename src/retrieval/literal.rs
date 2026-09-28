//! Case-sensitive exact substring matching over original UTF-8 bytes.
use std::ops::Range;

pub fn literal_matches(text: &str, query: &str) -> Vec<Range<usize>> {
    if query.is_empty() {
        return vec![];
    }
    text.match_indices(query)
        .take(64)
        .map(|(start, _)| start..start + query.len())
        .collect()
}

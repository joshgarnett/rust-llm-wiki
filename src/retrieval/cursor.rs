//! Stateless bounded cursors bind the complete snapshot and query contract.
use super::types::*;
use crate::{catalog::query_types::QueryCatalog, domain::*};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: u32,
    snapshot: ReadSnapshot,
    query: Blake3Hash,
    offset: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    publication: Option<String>,
}
pub(crate) fn fingerprint(query: &str, plan: &QueryPlan) -> Result<Blake3Hash> {
    // Ordering is part of pagination: old lexical cursors must not resume
    // a pool ranked under the positional phrase policy. Unchanged modes keep
    // their existing fingerprint contract.
    let policy = match plan.mode {
        SearchMode::Lexical | SearchMode::Hybrid => "lwiki-query-lexical-phrase-v2",
        SearchMode::Literal | SearchMode::Semantic => "lwiki-query-v1",
    };
    serde_json::to_vec(&(policy, query, plan.mode, &plan.filters, &plan.limits))
        .map(Blake3Hash::digest)
        .map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))
}
pub(crate) fn offset(
    reader: &dyn QueryCatalog,
    fingerprint: &Blake3Hash,
    cursor: Option<&str>,
    cap: usize,
) -> Result<usize> {
    let Some(value) = cursor else {
        return Ok(0);
    };
    if value.len() > 4096 || value.len() % 2 != 0 {
        return Err(stale("malformed pagination cursor"));
    }
    let bytes = value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let a = digit(pair[0])?;
            let b = digit(pair[1])?;
            Ok(a * 16 + b)
        })
        .collect::<Result<Vec<_>>>()?;
    let value: Cursor =
        serde_json::from_slice(&bytes).map_err(|_| stale("malformed pagination cursor"))?;
    if value.version != cursor_version(reader)
        || value.snapshot != *reader.snapshot()
        || value.publication.as_deref() != reader.publication_id()
        || &value.query != fingerprint
        || value.offset >= cap
    {
        return Err(stale("pagination query, filters or generation changed"));
    }
    Ok(value.offset)
}
pub(crate) fn encode(
    reader: &dyn QueryCatalog,
    fingerprint: Blake3Hash,
    offset: usize,
) -> Result<String> {
    let bytes = serde_json::to_vec(&Cursor {
        version: cursor_version(reader),
        snapshot: reader.snapshot().clone(),
        query: fingerprint,
        offset,
        publication: reader.publication_id().map(str::to_owned),
    })
    .map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn cursor_version(reader: &dyn QueryCatalog) -> u32 {
    if reader.snapshot().publication().is_some() {
        2
    } else {
        1
    }
}
fn digit(value: u8) -> Result<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(stale("malformed pagination cursor")),
    }
}
fn stale(message: &str) -> WikiError {
    WikiError::new(ErrorCode::CursorStale, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_lexical_order_invalidates_previous_query_fingerprints() {
        for mode in [
            SearchMode::Literal,
            SearchMode::Lexical,
            SearchMode::Semantic,
            SearchMode::Hybrid,
        ] {
            let plan = QueryPlan {
                mode,
                ..QueryPlan::default()
            };
            // Reproduce the previous executable's wire contract, rather than
            // issuing a second cursor under the current ranking policy.
            let previous = Blake3Hash::digest(
                serde_json::to_vec(&(
                    "lwiki-query-v1",
                    "alpha beta",
                    mode,
                    &plan.filters,
                    &plan.limits,
                ))
                .unwrap(),
            );
            let current = fingerprint("alpha beta", &plan).unwrap();
            match mode {
                SearchMode::Lexical | SearchMode::Hybrid => assert_ne!(current, previous),
                SearchMode::Literal | SearchMode::Semantic => assert_eq!(current, previous),
            }
        }
    }
}

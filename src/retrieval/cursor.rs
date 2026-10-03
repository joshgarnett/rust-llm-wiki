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
}
pub(crate) fn fingerprint(query: &str, plan: &QueryPlan) -> Result<Blake3Hash> {
    serde_json::to_vec(&(
        "lwiki-query-v1",
        query,
        plan.mode,
        &plan.filters,
        &plan.limits,
    ))
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
    if value.version != 1
        || value.snapshot != *reader.snapshot()
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
        version: 1,
        snapshot: reader.snapshot().clone(),
        query: fingerprint,
        offset,
    })
    .map_err(|e| WikiError::new(ErrorCode::Internal, e.to_string()))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
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

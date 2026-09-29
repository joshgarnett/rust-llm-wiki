use super::types::*;
use crate::{changes::prepare::strict_json, domain::*, graph::packet::canonical_json};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::collections::BTreeSet;

pub(crate) fn invalid(message: impl Into<String>) -> WikiError {
    WikiError::new(ErrorCode::RecordInvalid, message)
}
pub(crate) fn bound(text: &str, cap: usize, required: bool) -> Result<()> {
    if text.len() > cap || (required && text.trim().is_empty()) || text.contains('\0') {
        return Err(invalid(
            "research text is empty or exceeds its byte/content limit",
        ));
    }
    Ok(())
}
pub(crate) fn decode<T: DeserializeOwned>(bytes: &[u8], cap: usize) -> Result<T> {
    if bytes.is_empty() || bytes.len() > cap {
        return Err(invalid("research JSON byte limit exceeded"));
    }
    let value: Value = strict_json(bytes)?;
    let mut pending = vec![(&value, 0)];
    let mut count = 0;
    while let Some((v, depth)) = pending.pop() {
        count += 1;
        if count > 16384 || depth > 24 {
            return Err(invalid("research JSON depth/item limit exceeded"));
        }
        match v {
            Value::Array(a) => pending.extend(a.iter().map(|v| (v, depth + 1))),
            Value::Object(o) => pending.extend(o.values().map(|v| (v, depth + 1))),
            _ => {}
        }
    }
    serde_json::from_value(value).map_err(|_| invalid("research JSON does not match its schema"))
}
pub(crate) fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let bytes = canonical_json(value)?;
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Err(invalid("research artifact byte limit exceeded"));
    }
    Ok(bytes)
}
pub(crate) fn scope(scope: &ResearchScope) -> Result<()> {
    bound(&scope.question, 4096, true)?;
    if scope.urls.len() > 32
        || scope.exclusions.len() > 32
        || scope.source_ids.len() > MAX_PASSAGES
        || !(1..=8).contains(&scope.max_rounds)
        || scope.max_sources > 64
        || scope.max_source_bytes > 4 * 1024 * 1024
    {
        return Err(invalid("research scope exceeds its limits"));
    }
    for text in scope.urls.iter().chain(&scope.exclusions) {
        bound(text, 2048, true)?;
    }
    let unique: BTreeSet<_> = scope.source_ids.iter().collect();
    if unique.len() != scope.source_ids.len() {
        return Err(invalid("duplicate research source ID"));
    }
    Ok(())
}
pub(crate) fn fingerprint(packet: &ResearchPacket) -> Result<Blake3Hash> {
    let mut value = serde_json::to_value(packet).map_err(|_| invalid("packet encoding"))?;
    value
        .as_object_mut()
        .ok_or_else(|| invalid("packet object"))?
        .remove("packet_fingerprint");
    Ok(Blake3Hash::digest(encode(&value)?))
}
pub fn parse_submission(bytes: &[u8]) -> Result<ResearchSubmission> {
    let submission: ResearchSubmission = decode(bytes, MAX_SUBMISSION_BYTES)?;
    if submission.schema != "lwiki.research-submission.v1" {
        return Err(invalid("unsupported research submission schema"));
    }
    let gaps = match &submission.response {
        SubmissionContent::CollectSources { sources, gaps } => {
            if sources.len() > 32 {
                return Err(invalid("too many sources in one submission"));
            }
            let mut keys = BTreeSet::new();
            let mut total = 0usize;
            for source in sources {
                RecordId::new(&source.key)?;
                if !keys.insert(&source.key) {
                    return Err(invalid("duplicate submitted source key"));
                }
                bound(&source.title, 256, true)?;
                bound(&source.origin, 2048, true)?;
                bound(&source.content, MAX_PASSAGE_BYTES, true)?;
                if let Some(provenance) = &source.provenance {
                    bound(provenance, 2048, false)?;
                }
                total = total
                    .checked_add(source.content.len())
                    .ok_or_else(|| invalid("source bytes overflow"))?;
            }
            if total > MAX_PASSAGE_BYTES {
                return Err(invalid("submission source content exceeds 64 KiB"));
            }
            gaps
        }
        SubmissionContent::Answer {
            claims,
            gaps,
            follow_up,
        } => {
            if claims.len() > 32 {
                return Err(invalid("too many research claims"));
            }
            let mut total = 0;
            for claim in claims {
                bound(&claim.text, 4096, true)?;
                total += claim.text.len();
                let unique: BTreeSet<_> = claim.passage_ids.iter().collect();
                if unique.len() != claim.passage_ids.len() || unique.is_empty() || unique.len() > 16
                {
                    return Err(invalid("claims require 1–16 distinct packet passage IDs"));
                }
                for id in &claim.passage_ids {
                    bound(id, 16, true)?;
                }
            }
            if total > MAX_PASSAGE_BYTES {
                return Err(invalid("research claim text exceeds 64 KiB"));
            }
            if let Some(follow_up) = follow_up {
                bound(follow_up, 4096, true)?;
            }
            gaps
        }
    };
    if gaps.len() > 32 {
        return Err(invalid("too many research gaps"));
    }
    for gap in gaps {
        bound(gap, 2048, true)?;
    }
    Ok(submission)
}

//! Pure bounded retry decisions. Every retry still needs a new ledger reservation.
use super::types::*;
use crate::{domain::*, jobs::ClockReading};
use std::time::UNIX_EPOCH;

pub(super) struct NativeJitter;
impl JitterSource for NativeJitter {
    fn sample_inclusive(&self, max_ms: u64) -> Result<u64> {
        let range = max_ms
            .checked_add(1)
            .ok_or_else(|| WikiError::invalid("invalid jitter bound"))?;
        let threshold = range.wrapping_neg() % range;
        for _ in 0..128 {
            let mut bytes = [0u8; 8];
            getrandom::fill(&mut bytes)
                .map_err(|_| WikiError::new(ErrorCode::Internal, "jitter unavailable"))?;
            let value = u64::from_le_bytes(bytes);
            if value >= threshold {
                return Ok(value % range);
            }
        }
        Err(WikiError::new(ErrorCode::Internal, "jitter unavailable"))
    }
}

fn retry_after(headers: &[(String, String)], now: ClockReading) -> Option<u64> {
    let values: Vec<_> = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("retry-after"))
        .collect();
    if values.len() != 1 {
        return None;
    }
    let value = values[0].1.trim();
    if value.is_empty() || value.len() > 128 {
        return None;
    }
    if value.bytes().all(|b| b.is_ascii_digit()) {
        return Some(
            value
                .parse::<u64>()
                .ok()
                .and_then(|n| n.checked_mul(1000))
                .unwrap_or(u64::MAX),
        );
    }
    let date = httpdate::parse_http_date(value).ok()?;
    let millis = u64::try_from(date.duration_since(UNIX_EPOCH).ok()?.as_millis()).ok()?;
    let now = u64::try_from(now.utc_ms).ok()?;
    Some(millis.saturating_sub(now))
}

pub(super) fn decide(
    status: u16,
    headers: &[(String, String)],
    attempts: u32,
    now: ClockReading,
    deadline: i64,
    timeout_ms: u64,
    jitter: &dyn JitterSource,
) -> Result<RetryDecision> {
    if !matches!(status, 429 | 500 | 502 | 503 | 504) {
        return Ok(RetryDecision::Never);
    }
    let exponent = attempts.saturating_sub(1).min(5);
    let ceiling = (1000u64 << exponent).min(30_000);
    let sample = jitter.sample_inclusive(ceiling)?;
    if sample > ceiling {
        return Err(WikiError::invalid("jitter exceeded requested bound"));
    }
    let delay_ms = sample.max(retry_after(headers, now).unwrap_or(0));
    let fits = i64::try_from(delay_ms)
        .ok()
        .and_then(|delay| now.utc_ms.checked_add(delay))
        .and_then(|end| {
            i64::try_from(timeout_ms)
                .ok()
                .and_then(|timeout| end.checked_add(timeout))
        })
        .is_some_and(|end| end <= deadline);
    if !fits {
        return Ok(RetryDecision::Pause {
            reason: "retry_deadline".into(),
        });
    }
    Ok(RetryDecision::After {
        delay_ms,
        reason: if status == 429 {
            "rate_limit"
        } else {
            "server_error"
        }
        .into(),
    })
}

pub(super) fn uncertain(retry_uncertain: bool) -> RetryDecision {
    if retry_uncertain {
        RetryDecision::After {
            delay_ms: 0,
            reason: "explicit_uncertain_retry".into(),
        }
    } else {
        RetryDecision::Pause {
            reason: "outcome_unknown".into(),
        }
    }
}

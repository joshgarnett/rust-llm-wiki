//! Checked fixed-point admission and complete billable-class bounds.
use super::types::*;
use crate::domain::*;
use std::collections::BTreeMap;

fn overflow() -> WikiError {
    WikiError::new(ErrorCode::BudgetExceeded, "checked accounting overflow")
}
pub(super) fn invalid(message: &str) -> WikiError {
    WikiError::new(ErrorCode::ConfigInvalid, message)
}
impl MoneyParsing for Money {
    fn parse_decimal(currency: Currency, decimal: &str) -> Result<Money> {
        if decimal.is_empty() || decimal.len() > 30 {
            return Err(invalid("money decimal out of bounds"));
        }
        let mut parts = decimal.split('.');
        let whole = parts.next().unwrap_or_default();
        let fraction = parts.next().unwrap_or_default();
        if parts.next().is_some()
            || whole.is_empty()
            || !whole.bytes().all(|b| b.is_ascii_digit())
            || (whole.len() > 1 && whole.starts_with('0'))
            || fraction.len() > 9
            || !fraction.bytes().all(|b| b.is_ascii_digit())
            || (decimal.contains('.') && fraction.is_empty())
        {
            return Err(invalid(
                "money requires a nonnegative fixed decimal with at most nine fractional digits",
            ));
        }
        let whole = whole.parse::<u64>().map_err(|_| overflow())?;
        let mut nanos = whole.checked_mul(1_000_000_000).ok_or_else(overflow)?;
        if !fraction.is_empty() {
            let digits = fraction.parse::<u64>().map_err(|_| overflow())?;
            nanos = nanos
                .checked_add(
                    digits
                        .checked_mul(10u64.pow((9 - fraction.len()) as u32))
                        .ok_or_else(overflow)?,
                )
                .ok_or_else(overflow)?;
        }
        Ok(Money::new(currency, nanos))
    }
}
pub fn rate_card_fingerprint(card: &RateCard) -> Result<Blake3Hash> {
    let mut value = serde_json::to_value(card).map_err(|e| WikiError::invalid(e.to_string()))?;
    value.as_object_mut().unwrap().remove("fingerprint");
    Ok(Blake3Hash::digest(
        serde_json::to_vec(&value).map_err(|e| WikiError::invalid(e.to_string()))?,
    ))
}
pub fn bound_fingerprint(bound: &AttemptBound) -> Result<Blake3Hash> {
    let mut value = serde_json::to_value(bound).map_err(|e| WikiError::invalid(e.to_string()))?;
    value.as_object_mut().unwrap().remove("bounds_fingerprint");
    Ok(Blake3Hash::digest(
        serde_json::to_vec(&value).map_err(|e| WikiError::invalid(e.to_string()))?,
    ))
}
pub(super) fn count(bound: &TokenBound) -> Option<u64> {
    match bound {
        TokenBound::Exact { count, .. } | TokenBound::ProvenUpper { count, .. } => Some(*count),
        _ => None,
    }
}
pub fn validate_limits(limits: &LifetimeLimits) -> Result<()> {
    if limits.requests == 0
        || limits.concurrency == 0
        || limits.attempts_per_task == 0
        || limits.requests_per_minute == Some(0)
        || limits.tokens_per_minute == Some(0)
    {
        return Err(invalid(
            "request, attempt, concurrency and rate limits must be positive",
        ));
    }
    Ok(())
}
pub fn quote_bound(
    bound: &AttemptBound,
    limits: &LifetimeLimits,
    now_utc_ms: i64,
    deadline_utc_ms: i64,
) -> Result<Allowance> {
    validate_limits(limits)?;
    if bound.profile_id.is_empty()
        || bound.profile_id.len() > 128
        || bound.profile_id.chars().any(char::is_control)
        || bound.timeout_ms == 0
        || bound.bounds_fingerprint != bound_fingerprint(bound)?
    {
        return Err(invalid("invalid attempt profile, timeout or fingerprint"));
    }
    let response_cap = if bound.capability == Capability::Generate {
        GENERATION_SPOOL_MAX_BYTES
    } else {
        OTHER_SPOOL_MAX_BYTES
    };
    if bound.response_bytes == 0 || bound.response_bytes > response_cap {
        return Err(invalid("response bound exceeds sensitive spool ceiling"));
    }
    if now_utc_ms < 0 || deadline_utc_ms <= now_utc_ms {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "run deadline expired",
        ));
    }
    let end = now_utc_ms
        .checked_add(i64::try_from(bound.timeout_ms).map_err(|_| overflow())?)
        .ok_or_else(overflow)?;
    if end > deadline_utc_ms {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "attempt timeout exceeds remaining deadline",
        ));
    }
    if bound
        .applicable_classes
        .iter()
        .any(|class| !bound.billable_bounds.contains_key(class))
        || bound
            .billable_bounds
            .keys()
            .any(|class| !bound.applicable_classes.contains(class))
    {
        return Err(invalid(
            "billable class bounds do not match declared applicable classes",
        ));
    }
    let mut units = BTreeMap::new();
    for (class, value) in &bound.billable_bounds {
        if let Some(n) = count(value) {
            units.insert(*class, n);
        } else if limits.max_cost.is_some()
            || limits.billable_units.contains_key(class)
            || limits.tokens_per_minute.is_some()
        {
            return Err(WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "hard cost/token ceiling requires proven complete upper bounds",
            ));
        }
    }
    let cost = if let Some(card) = &bound.rate_card {
        if card.version == 0
            || card.id.is_empty()
            || card.id.len() > 128
            || card.fingerprint != rate_card_fingerprint(card)?
        {
            return Err(invalid("invalid rate card identity or fingerprint"));
        }
        let (from, until, locked) = match card.validity {
            PriceValidity::DispatchLocked {
                valid_from_utc_ms,
                valid_until_utc_ms,
            } => (valid_from_utc_ms, valid_until_utc_ms, true),
            PriceValidity::EntireAttempt {
                valid_from_utc_ms,
                valid_until_utc_ms,
            } => (valid_from_utc_ms, valid_until_utc_ms, false),
        };
        if from > now_utc_ms || until <= now_utc_ms || (!locked && until < end) {
            return Err(WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "rate card cannot cover the attempt billing interval",
            ));
        }
        let complete = bound
            .applicable_classes
            .iter()
            .all(|class| units.contains_key(class) && card.rates.contains_key(class));
        if complete {
            let mut amount = Money::new(card.currency.clone(), card.request_fee_nanounits);
            for class in &bound.applicable_classes {
                amount = amount.checked_add(&Money::new(
                    card.currency.clone(),
                    card.rates[class].allowance_nanounits(units[class])?,
                ))?;
            }
            Some(amount)
        } else if limits.max_cost.is_some() {
            return Err(WikiError::new(
                ErrorCode::CapabilityUnavailable,
                "hard cost requires complete applicable rates",
            ));
        } else {
            None
        }
    } else if limits.max_cost.is_some() {
        return Err(WikiError::new(
            ErrorCode::CapabilityUnavailable,
            "hard cost requires an applicable rate card",
        ));
    } else {
        None
    };
    if let Some(max) = &limits.max_cost
        && cost.as_ref().is_none_or(|c| c.currency() != max.currency())
    {
        return Err(invalid("hard cost currency differs from applicable rates"));
    }
    if cost != bound.quoted_allowance {
        return Err(invalid(
            "quoted cost disagrees with checked conservative rate calculation",
        ));
    }
    Ok(Allowance {
        requests: 1,
        request_bytes: bound.request_bytes,
        response_bytes: bound.response_bytes,
        billable_units: units,
        cost,
    })
}
pub(crate) fn zero() -> Allowance {
    Allowance {
        requests: 0,
        request_bytes: 0,
        response_bytes: 0,
        billable_units: BTreeMap::new(),
        cost: None,
    }
}
pub(crate) fn add(a: &mut Allowance, b: &Allowance) -> Result<()> {
    a.requests = a.requests.checked_add(b.requests).ok_or_else(overflow)?;
    a.request_bytes = a
        .request_bytes
        .checked_add(b.request_bytes)
        .ok_or_else(overflow)?;
    a.response_bytes = a
        .response_bytes
        .checked_add(b.response_bytes)
        .ok_or_else(overflow)?;
    for (class, count) in &b.billable_units {
        let prior = a.billable_units.entry(*class).or_default();
        *prior = prior.checked_add(*count).ok_or_else(overflow)?
    }
    a.cost = match (&a.cost, &b.cost) {
        (Some(a), Some(b)) if a.currency() == b.currency() => Some(a.checked_add(b)?),
        (Some(_), Some(_)) => None,
        (None, Some(b)) => Some(b.clone()),
        (Some(a), None) => Some(a.clone()),
        _ => None,
    };
    Ok(())
}
pub(crate) fn fits(
    settled: &Allowance,
    outstanding: &Allowance,
    proposed: &Allowance,
    limits: &LifetimeLimits,
) -> Result<()> {
    let mut total = settled.clone();
    add(&mut total, outstanding)?;
    add(&mut total, proposed)?;
    // Keep admission unchanged while naming the exhausted local resource. These
    // fixed literals survive the provider diagnostic boundary without exposing
    // arbitrary configuration, credentials, or provider response data.
    for (exceeded, message) in [
        (
            total.requests > limits.requests,
            "lifetime request reservation exceeds run budget",
        ),
        (
            limits
                .request_bytes
                .is_some_and(|n| total.request_bytes > n),
            "lifetime request-byte reservation exceeds run budget",
        ),
        (
            limits
                .response_bytes
                .is_some_and(|n| total.response_bytes > n),
            "lifetime response-byte reservation exceeds run budget",
        ),
    ] {
        if exceeded {
            return Err(WikiError::new(ErrorCode::BudgetExceeded, message));
        }
    }
    if limits
        .billable_units
        .iter()
        .any(|(c, n)| total.billable_units.get(c).copied().unwrap_or_default() > *n)
        || (total.requests > 0
            && limits.max_cost.as_ref().is_some_and(|max| {
                total.cost.as_ref().is_none_or(|v| {
                    v.currency() != max.currency() || v.nanounits() > max.nanounits()
                })
            }))
    {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "lifetime settled plus outstanding plus proposed exceeds run budget",
        ));
    }
    Ok(())
}
pub(super) fn usage_violates(bound: &AttemptBound, usage: &Usage) -> bool {
    usage
        .billable_units
        .keys()
        .any(|class| !bound.applicable_classes.contains(class))
        || usage.request_bytes > bound.request_bytes
        || usage.response_bytes > bound.response_bytes
        || usage
            .billable_units
            .iter()
            .any(|(class, value)| match value {
                KnownOrUnknown::Known(n) => {
                    bound
                        .billable_bounds
                        .get(class)
                        .and_then(count)
                        .is_some_and(|cap| *n > cap)
                        || !bound.applicable_classes.contains(class)
                }
                _ => false,
            })
}
pub(super) fn actual_allowance(
    bound: &AttemptBound,
    usage: &KnownOrUnknown<Usage>,
    cost: &KnownOrUnknown<Money>,
) -> Option<Allowance> {
    let KnownOrUnknown::Known(usage) = usage else {
        return None;
    };
    if !matches!(cost, KnownOrUnknown::Known(_)) {
        return None;
    }
    if usage
        .billable_units
        .keys()
        .any(|class| !bound.applicable_classes.contains(class))
    {
        return None;
    }
    let mut units = BTreeMap::new();
    for class in &bound.applicable_classes {
        let Some(KnownOrUnknown::Known(n)) = usage.billable_units.get(class) else {
            return None;
        };
        units.insert(*class, *n);
    }
    Some(Allowance {
        requests: 1,
        request_bytes: usage.request_bytes,
        response_bytes: usage.response_bytes,
        billable_units: units,
        cost: match cost {
            KnownOrUnknown::Known(v) => Some(v.clone()),
            _ => None,
        },
    })
}

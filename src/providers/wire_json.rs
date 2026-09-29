//! Bounded full-document JSON and paid usage observations. No fragment salvage.
use super::types::*;
use crate::{domain::*, jobs::*};
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};
use std::{collections::BTreeMap, fmt};

pub(super) fn parse(
    bytes: &[u8],
    max_bytes: usize,
    max_nodes: usize,
    max_depth: usize,
) -> Result<Value> {
    if bytes.len() > max_bytes {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "JSON byte ceiling",
        ));
    }
    struct Seed<'a> {
        remaining: &'a mut usize,
        depth: usize,
        maximum: usize,
    }
    impl<'de> DeserializeSeed<'de> for Seed<'_> {
        type Value = Value;
        fn deserialize<D: serde::Deserializer<'de>>(
            self,
            d: D,
        ) -> std::result::Result<Value, D::Error> {
            if *self.remaining == 0 || self.depth > self.maximum {
                return Err(serde::de::Error::custom("JSON structural ceiling"));
            }
            *self.remaining -= 1;
            d.deserialize_any(self)
        }
    }
    impl<'de> Visitor<'de> for Seed<'_> {
        type Value = Value;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("bounded JSON")
        }
        fn visit_bool<E: serde::de::Error>(self, v: bool) -> std::result::Result<Value, E> {
            Ok(Value::Bool(v))
        }
        fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<Value, E> {
            Ok(v.into())
        }
        fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<Value, E> {
            Ok(v.into())
        }
        fn visit_f64<E: serde::de::Error>(self, v: f64) -> std::result::Result<Value, E> {
            Number::from_f64(v)
                .map(Value::Number)
                .ok_or_else(|| E::custom("nonfinite JSON number"))
        }
        fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<Value, E> {
            Ok(Value::String(v.to_owned()))
        }
        fn visit_string<E: serde::de::Error>(self, v: String) -> std::result::Result<Value, E> {
            Ok(Value::String(v))
        }
        fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Value, E> {
            Ok(Value::Null)
        }
        fn visit_none<E: serde::de::Error>(self) -> std::result::Result<Value, E> {
            Ok(Value::Null)
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> std::result::Result<Value, A::Error> {
            let mut values = Vec::new();
            while let Some(value) = a.next_element_seed(Seed {
                remaining: self.remaining,
                depth: self.depth + 1,
                maximum: self.maximum,
            })? {
                values.push(value);
            }
            Ok(Value::Array(values))
        }
        fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> std::result::Result<Value, A::Error> {
            let mut values = Map::new();
            while let Some(key) = a.next_key::<String>()? {
                if values.contains_key(&key) {
                    return Err(serde::de::Error::custom("duplicate JSON key"));
                }
                let value = a.next_value_seed(Seed {
                    remaining: self.remaining,
                    depth: self.depth + 1,
                    maximum: self.maximum,
                })?;
                values.insert(key, value);
            }
            Ok(Value::Object(values))
        }
    }
    let mut remaining = max_nodes;
    let mut d = serde_json::Deserializer::from_slice(bytes);
    let value = Seed {
        remaining: &mut remaining,
        depth: 0,
        maximum: max_depth,
    }
    .deserialize(&mut d)
    .map_err(|_| WikiError::invalid("invalid or excessive JSON document"))?;
    d.end()
        .map_err(|_| WikiError::invalid("trailing JSON content"))?;
    Ok(value)
}

pub(super) fn reply_json(p: &PreparedWire, r: &TransportReply) -> Result<Value> {
    if r.body_exceeded
        || r.headers_exceeded
        || !r.terminal
        || r.observed_body_bytes < r.body.len() as u64
    {
        return Err(WikiError::invalid("incomplete provider response"));
    }
    parse(&r.body, p.bound.response_bytes as usize, 1_100_000, 32)
}
pub(super) fn field(v: &Value, key: &str, cap: usize) -> Option<String> {
    v.get(key)?
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= cap && !s.chars().any(char::is_control))
        .map(str::to_owned)
}
pub(super) fn expected_model(p: &PreparedWire) -> Option<&'static str> {
    let basis = match &p.contract {
        WireContract::Embedding(c) => &c.basis,
        WireContract::Generation(c) => &c.basis,
        #[cfg(test)]
        WireContract::Fixture => return None,
    };
    match basis {
        BoundBasis::UnknownCompatible => None,
        BoundBasis::OpenAiChatGpt41SnapshotV1 => Some("gpt-4.1-2025-04-14"),
        BoundBasis::OpenAiEmbedding3SmallV1 { .. } => Some("text-embedding-3-small"),
        BoundBasis::OpenAiEmbedding3LargeV1 { .. } => Some("text-embedding-3-large"),
    }
}
pub(super) fn returned_model(p: &PreparedWire, v: &Value) -> Result<String> {
    let model = field(v, "model", 256)
        .ok_or_else(|| WikiError::invalid("provider model missing or invalid"))?;
    if expected_model(p).is_some_and(|m| m != model) {
        return Err(WikiError::invalid(
            "returned model differs from documented contract",
        ));
    }
    Ok(model)
}
fn count(v: &Value, key: &str) -> Option<u64> {
    v.get(key).and_then(Value::as_u64)
}
fn known(n: Option<u64>) -> KnownOrUnknown<u64> {
    n.map_or(KnownOrUnknown::Unknown, KnownOrUnknown::Known)
}

/// Validity is independent from the knowledge result, and missing details never imply zero.
pub(super) fn usage_valid(v: &Value, generation: bool) -> bool {
    let Some(u) = v.get("usage") else { return true };
    if u.is_null() {
        return true;
    }
    if !u.is_object() {
        return false;
    }
    let allowed = if generation {
        &[
            "prompt_tokens",
            "completion_tokens",
            "total_tokens",
            "prompt_tokens_details",
            "completion_tokens_details",
        ][..]
    } else {
        &["prompt_tokens", "total_tokens"][..]
    };
    if u.as_object()
        .is_some_and(|m| m.keys().any(|k| !allowed.contains(&k.as_str())))
    {
        return false;
    }
    let scalar = |o: &Value, k: &str| o.get(k).is_none_or(|n| n.is_null() || n.as_u64().is_some());
    if !["prompt_tokens", "completion_tokens", "total_tokens"]
        .iter()
        .all(|k| scalar(u, k))
    {
        return false;
    }
    let prompt = count(u, "prompt_tokens");
    let completion = count(u, "completion_tokens");
    if let Some(total) = count(u, "total_tokens") {
        let expected = if generation {
            prompt.zip(completion).and_then(|(a, b)| a.checked_add(b))
        } else {
            prompt
        };
        if expected.is_some_and(|n| n != total)
            || (generation && prompt.is_some() && completion.is_some() && expected.is_none())
        {
            return false;
        }
    }
    for (key, total, fields) in [
        (
            "prompt_tokens_details",
            prompt,
            &["cached_tokens", "audio_tokens"][..],
        ),
        (
            "completion_tokens_details",
            completion,
            &[
                "reasoning_tokens",
                "audio_tokens",
                "accepted_prediction_tokens",
                "rejected_prediction_tokens",
            ][..],
        ),
    ] {
        if let Some(d) = u.get(key).filter(|d| !d.is_null()) {
            if !d.is_object()
                || !fields.iter().all(|k| scalar(d, k))
                || d.as_object()
                    .is_some_and(|m| m.keys().any(|k| !fields.contains(&k.as_str())))
            {
                return false;
            }
            if fields.iter().any(|k| {
                count(d, k)
                    .zip(total)
                    .is_some_and(|(subset, total)| subset > total)
            }) {
                return false;
            }
        }
    }
    true
}
pub(super) fn supported_usage(p: &PreparedWire, v: &Value, generation: bool) -> bool {
    let Some(u) = v.get("usage").filter(|u| u.is_object()) else {
        return true;
    };
    if !generation {
        return count(u, "completion_tokens").is_none_or(|n| n == 0);
    }
    let input = count(u, "prompt_tokens");
    let output = count(u, "completion_tokens");
    let forbidden = [
        ("prompt_tokens_details", "audio_tokens"),
        ("completion_tokens_details", "audio_tokens"),
        ("completion_tokens_details", "accepted_prediction_tokens"),
        ("completion_tokens_details", "rejected_prediction_tokens"),
    ]
    .iter()
    .any(|(d, k)| {
        let total = if *d == "prompt_tokens_details" {
            input
        } else {
            output
        };
        u.get(d)
            .and_then(|d| count(d, k))
            .filter(|n| total.is_none_or(|t| *n <= t))
            .is_some_and(|n| n > 0)
    });
    let reasoning = u
        .get("completion_tokens_details")
        .and_then(|d| count(d, "reasoning_tokens"))
        .filter(|n| output.is_none_or(|t| *n <= t));
    !forbidden && !(expected_model(p).is_some() && reasoning.is_some_and(|n| n > 0))
}
pub(super) fn totals_within_contract(p: &PreparedWire, v: &Value) -> bool {
    let generation = matches!(p.contract, WireContract::Generation(_));
    if !usage_valid(v, generation) {
        return true;
    }
    let Some(u) = v.get("usage").filter(|u| u.is_object()) else {
        return true;
    };
    match &p.contract {
        WireContract::Generation(c) if matches!(c.basis, BoundBasis::OpenAiChatGpt41SnapshotV1) => {
            let combined = 1_047_576u64.checked_add(c.total_generated_token_limit);
            !count(u, "prompt_tokens").is_some_and(|n| n > 1_047_576)
                && !count(u, "completion_tokens").is_some_and(|n| n > c.total_generated_token_limit)
                && !count(u, "total_tokens")
                    .zip(combined)
                    .is_some_and(|(n, max)| n > max)
        }
        WireContract::Embedding(c)
            if matches!(
                c.basis,
                BoundBasis::OpenAiEmbedding3SmallV1 { .. }
                    | BoundBasis::OpenAiEmbedding3LargeV1 { .. }
            ) =>
        {
            let maximum = (c.items.len() as u64)
                .checked_mul(8192)
                .map(|n| n.min(300_000));
            !["prompt_tokens", "total_tokens"]
                .iter()
                .any(|key| count(u, key).zip(maximum).is_some_and(|(n, max)| n > max))
        }
        _ => true,
    }
}
pub(super) fn observe(p: &PreparedWire, r: &TransportReply, generation: bool) -> ObservedUsage {
    let mut units = p
        .bound
        .applicable_classes
        .iter()
        .map(|c| (*c, KnownOrUnknown::Unknown))
        .collect::<BTreeMap<_, _>>();
    let parsed = reply_json(p, r).ok();
    let model = parsed.as_ref().and_then(|v| field(v, "model", 256));
    let mut violation = if model
        .as_deref()
        .zip(expected_model(p))
        .is_some_and(|(a, b)| a != b)
    {
        Some(WireContractViolation::ReturnedModelMismatch)
    } else {
        None
    };
    if let Some(v) = &parsed
        && let Some(u) = v.get("usage").filter(|u| u.is_object())
    {
        let input = count(u, "prompt_tokens");
        if !totals_within_contract(p, v) {
            violation = Some(WireContractViolation::ObservedTotalBoundExceeded);
        }
        if generation {
            let cached = u
                .get("prompt_tokens_details")
                .and_then(|d| count(d, "cached_tokens"));
            let output = count(u, "completion_tokens");
            let reasoning = u
                .get("completion_tokens_details")
                .and_then(|d| count(d, "reasoning_tokens"));
            let valid_input = input.zip(cached).filter(|(t, c)| c <= t);
            let valid_output = output.zip(reasoning).filter(|(t, c)| c <= t);
            units.insert(BillableClass::Input, known(valid_input.map(|(t, c)| t - c)));
            units.insert(
                BillableClass::CachedInput,
                known(valid_input.map(|(_, c)| c)),
            );
            units.insert(
                BillableClass::Output,
                known(valid_output.map(|(t, c)| t - c)),
            );
            units.insert(
                BillableClass::Reasoning,
                known(valid_output.map(|(_, c)| c)),
            );
        } else {
            units.insert(BillableClass::Input, known(input));
        }
        if !supported_usage(p, v, generation) {
            violation = Some(WireContractViolation::UnexpectedBillableClass)
        }
    }
    let mut cost = KnownOrUnknown::Unknown;
    if violation.is_none()
        && parsed.as_ref().is_some_and(|v| usage_valid(v, generation))
        && let Some(card) = &p.bound.rate_card
    {
        let total = units
            .iter()
            .try_fold(card.request_fee_nanounits, |sum, (class, n)| {
                let KnownOrUnknown::Known(n) = n else {
                    return None;
                };
                sum.checked_add(card.rates.get(class)?.allowance_nanounits(*n).ok()?)
            });
        if let Some(total) = total {
            cost = KnownOrUnknown::Known(Money::new(card.currency.clone(), total));
        }
    }
    let header_ids = r
        .headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("x-request-id"))
        .collect::<Vec<_>>();
    let header_id = if header_ids.len() == 1 {
        let value = &header_ids[0].1;
        (!value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control))
            .then(|| value.clone())
    } else {
        None
    };
    ObservedUsage {
        usage: KnownOrUnknown::Known(Usage {
            billable_units: units,
            request_bytes: p.body.len() as u64,
            response_bytes: r.observed_body_bytes,
        }),
        computed_cost: cost,
        provider_request_id: parsed
            .as_ref()
            .and_then(|v| field(v, "id", 128))
            .or(header_id),
        returned_model: model,
        contract_violation: violation,
    }
}

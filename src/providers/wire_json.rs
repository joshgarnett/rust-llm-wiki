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
        WireContract::Search(_) => return None,
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

#[derive(Clone, Copy)]
enum UsageSurface {
    Embedding,
    Chat,
    Responses,
}
fn usage_surface(p: &PreparedWire, generation: bool) -> UsageSurface {
    if !generation {
        return UsageSurface::Embedding;
    }
    match &p.contract {
        WireContract::Generation(c) if c.surface == GenerationSurface::Responses => {
            UsageSurface::Responses
        }
        _ => UsageSurface::Chat,
    }
}
fn usage_keys(surface: UsageSurface) -> (&'static str, &'static str, &'static str, &'static str) {
    match surface {
        UsageSurface::Embedding | UsageSurface::Chat => (
            "prompt_tokens",
            "completion_tokens",
            "prompt_tokens_details",
            "completion_tokens_details",
        ),
        UsageSurface::Responses => (
            "input_tokens",
            "output_tokens",
            "input_tokens_details",
            "output_tokens_details",
        ),
    }
}
struct UsageAssessment {
    valid: bool,
    uncertain_cost: bool,
    extra_billable: bool,
    forbidden_class: bool,
    input: Option<u64>,
    output: Option<u64>,
    cached: Option<u64>,
    reasoning: Option<u64>,
}
fn assess_usage(v: &Value, surface: UsageSurface) -> UsageAssessment {
    let mut result = UsageAssessment {
        valid: true,
        uncertain_cost: false,
        extra_billable: false,
        forbidden_class: false,
        input: None,
        output: None,
        cached: None,
        reasoning: None,
    };
    let Some(u) = v.get("usage").filter(|u| !u.is_null()) else {
        return result;
    };
    let Some(fields) = u.as_object() else {
        result.valid = false;
        return result;
    };
    let (input_key, output_key, input_detail_key, output_detail_key) = usage_keys(surface);
    let scalar = |o: &Value, key: &str| {
        o.get(key)
            .is_none_or(|value| value.is_null() || value.as_u64().is_some())
    };
    let known_top = [
        input_key,
        output_key,
        "total_tokens",
        input_detail_key,
        output_detail_key,
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
        "cost",
    ];
    result.uncertain_cost = fields.keys().any(|key| !known_top.contains(&key.as_str()))
        || u.get("cost").is_some_and(|cost| !cost.is_null());
    if ![
        input_key,
        output_key,
        "total_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
    ]
    .iter()
    .all(|key| scalar(u, key))
    {
        result.valid = false;
        return result;
    }
    result.input = count(u, input_key);
    result.output = count(u, output_key);
    if let Some(total) = count(u, "total_tokens") {
        let expected = match surface {
            UsageSurface::Embedding => result.input.zip(Some(result.output.unwrap_or(0))),
            _ => result.input.zip(result.output),
        }
        .and_then(|(input, output)| input.checked_add(output));
        if expected.is_some_and(|sum| sum != total)
            || (result.input.is_some()
                && (matches!(surface, UsageSurface::Embedding) || result.output.is_some())
                && expected.is_none())
        {
            result.valid = false;
            return result;
        }
    }
    let input_details = u.get(input_detail_key).filter(|value| !value.is_null());
    let output_details = u.get(output_detail_key).filter(|value| !value.is_null());
    let input_fields = [
        "cached_tokens",
        "audio_tokens",
        "text_tokens",
        "cache_write_tokens",
        "cache_creation_tokens",
    ];
    let output_fields = [
        "reasoning_tokens",
        "audio_tokens",
        "text_tokens",
        "accepted_prediction_tokens",
        "rejected_prediction_tokens",
    ];
    for (details, parent, known_fields) in [
        (input_details, result.input, &input_fields[..]),
        (output_details, result.output, &output_fields[..]),
    ] {
        if let Some(details) = details {
            let Some(fields) = details.as_object() else {
                result.valid = false;
                return result;
            };
            if known_fields.iter().any(|key| !scalar(details, key))
                || known_fields
                    .iter()
                    .any(|key| count(details, key).zip(parent).is_some_and(|(n, p)| n > p))
            {
                result.valid = false;
                return result;
            }
            result.uncertain_cost |= fields
                .keys()
                .any(|key| !known_fields.contains(&key.as_str()));
        }
    }
    result.cached = input_details.and_then(|details| count(details, "cached_tokens"));
    result.reasoning = output_details.and_then(|details| count(details, "reasoning_tokens"));
    let read_alias = count(u, "cache_read_input_tokens");
    let creation_alias = count(u, "cache_creation_input_tokens");
    let creation_detail = input_details.and_then(|details| count(details, "cache_creation_tokens"));
    if result.cached.zip(read_alias).is_some_and(|(a, b)| a != b)
        || creation_detail
            .zip(creation_alias)
            .is_some_and(|(a, b)| a != b)
        || read_alias
            .zip(result.input)
            .is_some_and(|(subset, parent)| subset > parent)
        || creation_alias
            .zip(result.input)
            .is_some_and(|(subset, parent)| subset > parent)
    {
        result.valid = false;
        return result;
    }
    let nonoverlap = |left: Option<u64>, right: Option<u64>, parent: Option<u64>| {
        left.zip(right).is_none_or(|(a, b)| {
            a.checked_add(b)
                .is_some_and(|sum| parent.is_none_or(|total| sum <= total))
        })
    };
    let write = input_details.and_then(|details| count(details, "cache_write_tokens"));
    let created = creation_detail.into_iter().chain(creation_alias).max();
    let read = result.cached.or(read_alias);
    let text_output = output_details.and_then(|details| count(details, "text_tokens"));
    if !nonoverlap(read, write.into_iter().chain(created).max(), result.input)
        || !nonoverlap(result.reasoning, text_output, result.output)
    {
        result.valid = false;
        return result;
    }
    let positive = |details: Option<&Value>, key: &str| {
        details
            .and_then(|value| count(value, key))
            .is_some_and(|n| n > 0)
    };
    result.extra_billable = positive(input_details, "cache_write_tokens")
        || positive(input_details, "cache_creation_tokens")
        || creation_alias.is_some_and(|n| n > 0)
        || matches!(surface, UsageSurface::Embedding)
            && (result.cached.is_some_and(|n| n > 0) || read_alias.is_some_and(|n| n > 0));
    result.forbidden_class = (matches!(surface, UsageSurface::Embedding)
        && output_fields
            .iter()
            .any(|key| positive(output_details, key)))
        || positive(input_details, "audio_tokens")
        || positive(output_details, "audio_tokens")
        || positive(output_details, "accepted_prediction_tokens")
        || positive(output_details, "rejected_prediction_tokens");
    result.uncertain_cost |= result.extra_billable;
    result
}

/// Validity is independent from accounting completeness; missing details never imply zero.
pub(super) fn usage_valid(v: &Value, generation: bool) -> bool {
    assess_usage(
        v,
        if generation {
            UsageSurface::Chat
        } else {
            UsageSurface::Embedding
        },
    )
    .valid
}
pub(super) fn usage_valid_for(p: &PreparedWire, v: &Value, generation: bool) -> bool {
    assess_usage(v, usage_surface(p, generation)).valid
}
pub(super) fn supported_usage(p: &PreparedWire, v: &Value, generation: bool) -> bool {
    let surface = usage_surface(p, generation);
    let analysis = assess_usage(v, surface);
    supports_usage(&analysis, surface, expected_model(p).is_some())
}
fn supports_usage(analysis: &UsageAssessment, surface: UsageSurface, documented: bool) -> bool {
    if !analysis.valid {
        return true;
    }
    if matches!(surface, UsageSurface::Embedding) && analysis.output.is_some_and(|n| n > 0) {
        return false;
    }
    !analysis.forbidden_class
        && !(documented && (analysis.extra_billable || analysis.reasoning.is_some_and(|n| n > 0)))
}
pub(super) fn totals_within_contract(p: &PreparedWire, v: &Value) -> bool {
    let generation = matches!(p.contract, WireContract::Generation(_));
    if !usage_valid_for(p, v, generation) {
        return true;
    }
    let Some(u) = v.get("usage").filter(|u| u.is_object()) else {
        return true;
    };
    let (input_key, output_key, _, _) = usage_keys(usage_surface(p, generation));
    match &p.contract {
        WireContract::Generation(c) if matches!(c.basis, BoundBasis::OpenAiChatGpt41SnapshotV1) => {
            let combined = 1_047_576u64.checked_add(c.total_generated_token_limit);
            !count(u, input_key).is_some_and(|n| n > 1_047_576)
                && !count(u, output_key).is_some_and(|n| n > c.total_generated_token_limit)
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
            ![input_key, "total_tokens"]
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
        && v.get("usage").is_some_and(Value::is_object)
    {
        let analysis = assess_usage(v, usage_surface(p, generation));
        let input = analysis.input;
        if !totals_within_contract(p, v) {
            violation = Some(WireContractViolation::ObservedTotalBoundExceeded);
        }
        if generation {
            let cached = analysis.cached;
            let output = analysis.output;
            let reasoning = analysis.reasoning;
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
        && parsed.as_ref().is_some_and(|v| {
            let analysis = assess_usage(v, usage_surface(p, generation));
            analysis.valid && !analysis.uncertain_cost
        })
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

#[cfg(test)]
mod usage_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reported_embedding_chat_and_responses_usage_remain_valid() {
        let embedding = json!({"usage":{"completion_tokens":0,"prompt_tokens":6,"total_tokens":6,"completion_tokens_details":null,"prompt_tokens_details":null}});
        let a = assess_usage(&embedding, UsageSurface::Embedding);
        assert!(a.valid);
        assert_eq!(a.input, Some(6));
        assert_eq!(a.output, Some(0));
        assert!(supports_usage(&a, UsageSurface::Embedding, false));

        let claude = json!({"usage":{"completion_tokens":17,"prompt_tokens":22,"total_tokens":39,"completion_tokens_details":{"reasoning_tokens":0,"text_tokens":17},"prompt_tokens_details":{"cached_tokens":0,"text_tokens":22,"cache_write_tokens":0,"cache_creation_tokens":0},"cache_creation_input_tokens":0,"cache_read_input_tokens":0}});
        let a = assess_usage(&claude, UsageSurface::Chat);
        assert!(a.valid && !a.uncertain_cost);
        assert_eq!(
            (a.input, a.cached, a.output, a.reasoning),
            (Some(22), Some(0), Some(17), Some(0))
        );

        let gpt_chat = json!({"usage":{"completion_tokens":32,"prompt_tokens":24,"total_tokens":56,"completion_tokens_details":{"reasoning_tokens":21},"prompt_tokens_details":{"cached_tokens":0,"cache_write_tokens":0,"cache_creation_tokens":0}}});
        let a = assess_usage(&gpt_chat, UsageSurface::Chat);
        assert!(a.valid && !a.uncertain_cost);
        assert_eq!((a.output, a.reasoning), (Some(32), Some(21)));

        let responses = json!({"usage":{"input_tokens":47,"input_tokens_details":{"audio_tokens":null,"cached_tokens":0,"text_tokens":null,"cache_write_tokens":0},"output_tokens":50,"output_tokens_details":{"reasoning_tokens":34,"text_tokens":null},"total_tokens":97,"cost":null}});
        let a = assess_usage(&responses, UsageSurface::Responses);
        assert!(a.valid && !a.uncertain_cost);
        assert_eq!(
            (a.input, a.cached, a.output, a.reasoning),
            (Some(47), Some(0), Some(50), Some(34))
        );

        let claude_responses = json!({"usage":{"input_tokens":163,"input_tokens_details":{"audio_tokens":null,"cached_tokens":0,"text_tokens":163},"output_tokens":8,"output_tokens_details":{"reasoning_tokens":0,"text_tokens":8},"total_tokens":171,"cost":null}});
        assert!(assess_usage(&claude_responses, UsageSurface::Responses).valid);
    }

    #[test]
    fn malformed_core_counts_aliases_and_overlap_do_not_validate() {
        for usage in [
            json!({"prompt_tokens":3,"completion_tokens":-1,"total_tokens":3}),
            json!({"prompt_tokens":3,"completion_tokens":2,"total_tokens":4}),
            json!({"prompt_tokens":u64::MAX,"completion_tokens":1,"total_tokens":0}),
            json!({"prompt_tokens":5,"prompt_tokens_details":{"cached_tokens":6}}),
            json!({"prompt_tokens":5,"prompt_tokens_details":{"cached_tokens":2},"cache_read_input_tokens":3}),
            json!({"prompt_tokens":5,"prompt_tokens_details":{"cached_tokens":4,"cache_write_tokens":2}}),
            json!({"completion_tokens":5,"completion_tokens_details":{"reasoning_tokens":4,"text_tokens":2}}),
            json!({"prompt_tokens":5,"prompt_tokens_details":{"cache_creation_tokens":2},"cache_creation_input_tokens":3}),
        ] {
            assert!(!assess_usage(&json!({"usage":usage}), UsageSurface::Chat).valid);
        }
        let duplicate = br#"{"usage":{"prompt_tokens":1,"prompt_tokens":2}}"#;
        assert!(parse(duplicate, 1024, 128, 8).is_err());
    }

    #[test]
    fn extensions_preserve_output_but_not_complete_cost() {
        let mut usage = json!({"usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12,"prompt_tokens_details":{"cached_tokens":0},"completion_tokens_details":{"reasoning_tokens":0}}});
        assert!(!assess_usage(&usage, UsageSurface::Chat).uncertain_cost);
        usage["usage"]["provider_latency_ms"] = json!(4);
        let a = assess_usage(&usage, UsageSurface::Chat);
        assert!(a.valid && a.uncertain_cost);
        assert!(supports_usage(&a, UsageSurface::Chat, true));
        usage["usage"]
            .as_object_mut()
            .unwrap()
            .remove("provider_latency_ms");
        usage["usage"]["prompt_tokens_details"]["cache_write_tokens"] = json!(2);
        let a = assess_usage(&usage, UsageSurface::Chat);
        assert!(a.valid && a.uncertain_cost && a.extra_billable);
        assert!(supports_usage(&a, UsageSurface::Chat, false));
        assert!(!supports_usage(&a, UsageSurface::Chat, true));
        usage["usage"]["prompt_tokens_details"]["cache_write_tokens"] = json!(0);
        usage["usage"]["cost"] = json!(0.00001);
        let a = assess_usage(&usage, UsageSurface::Chat);
        assert!(a.valid && a.uncertain_cost);
        usage["usage"]["cost"] = Value::Null;
        assert!(!assess_usage(&usage, UsageSurface::Chat).uncertain_cost);
    }

    #[test]
    fn embedding_positive_output_subsets_cannot_hide_behind_missing_parent() {
        for key in [
            "reasoning_tokens",
            "text_tokens",
            "audio_tokens",
            "accepted_prediction_tokens",
        ] {
            let usage = json!({"usage":{"prompt_tokens":6,"total_tokens":6,
                "completion_tokens_details":{key:1}}});
            let a = assess_usage(&usage, UsageSurface::Embedding);
            assert!(a.valid);
            assert!(!supports_usage(&a, UsageSurface::Embedding, false));
        }
    }

    #[test]
    fn missing_partitions_stay_unknown_and_embedding_completion_is_unsupported() {
        let incomplete = json!({"usage":{"input_tokens":24,"input_tokens_details":{"cached_tokens":0},"output_tokens":16,"output_tokens_details":{"reasoning_tokens":16},"total_tokens":40}});
        let a = assess_usage(&incomplete, UsageSurface::Responses);
        assert!(a.valid);
        assert_eq!(
            (a.input, a.cached, a.output, a.reasoning),
            (Some(24), Some(0), Some(16), Some(16))
        );
        let missing = json!({"usage":{"input_tokens":24,"output_tokens":16,"total_tokens":40}});
        let a = assess_usage(&missing, UsageSurface::Responses);
        assert!(a.valid);
        assert_eq!((a.cached, a.reasoning), (None, None));
        let embedding = json!({"usage":{"prompt_tokens":6,"completion_tokens":1,"total_tokens":7}});
        let a = assess_usage(&embedding, UsageSurface::Embedding);
        assert!(a.valid);
        assert!(!supports_usage(&a, UsageSurface::Embedding, false));
    }
}

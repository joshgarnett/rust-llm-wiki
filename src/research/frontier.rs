//! Strict untrusted frontier proposals. Validation never authorizes remote work.
use crate::{
    domain::*,
    providers::{public_fetch, search_wire},
};
use serde::{
    Deserialize, Serialize,
    de::{DeserializeOwned, DeserializeSeed, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Number, Value, json};
use std::{collections::BTreeSet, fmt};

/// Caller-supplied ceilings, bounded by the published stage contracts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageLimits {
    pub max_bytes: usize,
    pub max_nodes: usize,
    pub max_depth: usize,
    pub max_queries: usize,
    pub max_urls: usize,
    pub max_sections: usize,
    pub max_claims: usize,
    pub max_proposals: usize,
    pub max_citations: usize,
    pub max_strings: usize,
    pub max_text_bytes: usize,
    pub max_heading_bytes: usize,
    pub max_url_bytes: usize,
}
impl Default for StageLimits {
    fn default() -> Self {
        Self {
            max_bytes: 256 * 1024,
            max_nodes: 16_384,
            max_depth: 16,
            max_queries: 20,
            max_urls: 60,
            max_sections: 16,
            max_claims: 64,
            max_proposals: 16,
            max_citations: 256,
            max_strings: 64,
            max_text_bytes: 64 * 1024,
            max_heading_bytes: 1024,
            max_url_bytes: 8192,
        }
    }
}
impl StageLimits {
    pub fn validate(&self) -> Result<()> {
        let default = Self::default();
        let actual = [
            self.max_bytes,
            self.max_nodes,
            self.max_depth,
            self.max_queries,
            self.max_urls,
            self.max_sections,
            self.max_claims,
            self.max_proposals,
            self.max_citations,
            self.max_strings,
            self.max_text_bytes,
            self.max_heading_bytes,
            self.max_url_bytes,
        ];
        let maximum = [
            default.max_bytes,
            default.max_nodes,
            default.max_depth,
            default.max_queries,
            default.max_urls,
            default.max_sections,
            default.max_claims,
            default.max_proposals,
            default.max_citations,
            default.max_strings,
            default.max_text_bytes,
            default.max_heading_bytes,
            default.max_url_bytes,
        ];
        if actual.iter().zip(maximum).any(|(a, m)| *a > m)
            || self.max_bytes == 0
            || self.max_nodes == 0
            || self.max_depth == 0
        {
            return Err(WikiError::invalid(
                "research stage limits exceed schema ceilings",
            ));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frontier {
    pub queries: Vec<String>,
    pub urls: Vec<String>,
    pub reason: String,
}
pub fn validate(bytes: &[u8], limits: &StageLimits, exclusions: &[String]) -> Result<Frontier> {
    let output: Frontier = parse(bytes, limits)?;
    leads(&output.queries, &output.urls, limits, exclusions)?;
    text(&output.reason, limits.max_text_bytes, false)?;
    Ok(output)
}
pub(crate) fn count(actual: usize, maximum: usize) -> Result<()> {
    if actual > maximum {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "research stage item ceiling",
        ));
    }
    Ok(())
}
pub(crate) fn text(value: &str, maximum: usize, empty: bool) -> Result<()> {
    if value.len() > maximum
        || (!empty && value.trim().is_empty())
        || value
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err(WikiError::invalid(
            "research stage text is empty, excessive, or contains controls",
        ));
    }
    Ok(())
}
pub(crate) fn leads(
    queries: &[String],
    urls: &[String],
    limits: &StageLimits,
    exclusions: &[String],
) -> Result<()> {
    count(queries.len(), limits.max_queries)?;
    count(urls.len(), limits.max_urls)?;
    count(exclusions.len(), limits.max_strings)?;
    for excluded in exclusions {
        text(excluded, limits.max_heading_bytes, false)?;
    }
    let mut seen = BTreeSet::new();
    for query in queries {
        search_wire::validate(query, 1, 0)?;
        if !seen.insert(query.to_lowercase()) {
            return Err(WikiError::invalid("duplicate research query"));
        }
        excluded(query, exclusions)?;
    }
    seen.clear();
    for raw in urls {
        count(raw.len(), limits.max_url_bytes)?;
        let url = public_fetch::validate_url(raw, None)?;
        if !seen.insert(url.to_string()) {
            return Err(WikiError::invalid("duplicate research URL"));
        }
        excluded(raw, exclusions)?;
        excluded(url.as_str(), exclusions)?;
    }
    Ok(())
}
fn excluded(value: &str, exclusions: &[String]) -> Result<()> {
    let value = value.to_lowercase();
    if exclusions
        .iter()
        .any(|s| value.contains(&s.trim().to_lowercase()))
    {
        return Err(WikiError::invalid("research lead matches caller exclusion"));
    }
    Ok(())
}

// Parse before DTO construction: bytes, nesting, values, keys and duplicate keys are bounded.
pub(crate) fn parse_value(bytes: &[u8], limits: &StageLimits) -> Result<Value> {
    limits.validate()?;
    count(bytes.len(), limits.max_bytes)?;
    struct Seed<'a> {
        remaining: &'a mut usize,
        depth: usize,
        limits: &'a StageLimits,
    }
    impl<'de> DeserializeSeed<'de> for Seed<'_> {
        type Value = Value;
        fn deserialize<D: serde::Deserializer<'de>>(
            self,
            d: D,
        ) -> std::result::Result<Value, D::Error> {
            if *self.remaining == 0 || self.depth > self.limits.max_depth {
                return Err(serde::de::Error::custom("JSON structural ceiling"));
            }
            *self.remaining -= 1;
            d.deserialize_any(self)
        }
    }
    impl<'de> Visitor<'de> for Seed<'_> {
        type Value = Value;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("bounded research JSON")
        }
        fn visit_bool<E: serde::de::Error>(self, v: bool) -> std::result::Result<Value, E> {
            Ok(Value::Bool(v))
        }
        fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<Value, E> {
            Ok(v.into())
        }
        fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<Value, E> {
            Ok(v.into())
        }
        fn visit_f64<E: serde::de::Error>(self, v: f64) -> std::result::Result<Value, E> {
            Number::from_f64(v)
                .map(Value::Number)
                .ok_or_else(|| E::custom("nonfinite JSON"))
        }
        fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Value, E> {
            Ok(Value::Null)
        }
        fn visit_none<E: serde::de::Error>(self) -> std::result::Result<Value, E> {
            Ok(Value::Null)
        }
        fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<Value, E> {
            if v.len() > self.limits.max_text_bytes {
                return Err(E::custom("JSON string ceiling"));
            }
            Ok(Value::String(v.into()))
        }
        fn visit_string<E: serde::de::Error>(self, v: String) -> std::result::Result<Value, E> {
            if v.len() > self.limits.max_text_bytes {
                return Err(E::custom("JSON string ceiling"));
            }
            Ok(Value::String(v))
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> std::result::Result<Value, A::Error> {
            let mut values = Vec::new();
            while let Some(value) = a.next_element_seed(Seed {
                remaining: self.remaining,
                depth: self.depth + 1,
                limits: self.limits,
            })? {
                values.push(value);
            }
            Ok(Value::Array(values))
        }
        fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> std::result::Result<Value, A::Error> {
            let mut values = Map::new();
            while let Some(key) = a.next_key::<String>()? {
                if key.len() > 128 || values.contains_key(&key) || *self.remaining == 0 {
                    return Err(serde::de::Error::custom("JSON key ceiling or duplicate"));
                }
                *self.remaining -= 1;
                let value = a.next_value_seed(Seed {
                    remaining: self.remaining,
                    depth: self.depth + 1,
                    limits: self.limits,
                })?;
                values.insert(key, value);
            }
            Ok(Value::Object(values))
        }
    }
    let mut remaining = limits.max_nodes;
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let value = Seed {
        remaining: &mut remaining,
        depth: 0,
        limits,
    }
    .deserialize(&mut decoder)
    .map_err(|_| WikiError::invalid("invalid or excessive research JSON"))?;
    decoder
        .end()
        .map_err(|_| WikiError::invalid("trailing research JSON"))?;
    Ok(value)
}
pub(crate) fn parse<T: DeserializeOwned>(bytes: &[u8], limits: &StageLimits) -> Result<T> {
    serde_json::from_value(parse_value(bytes, limits)?)
        .map_err(|_| WikiError::invalid("research stage schema mismatch"))
}
pub(crate) fn object(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
pub(crate) fn string(maximum: usize, empty: bool) -> Value {
    json!({"type":"string","minLength":usize::from(!empty),"maxLength":maximum})
}
pub(crate) fn array(items: Value, maximum: usize) -> Value {
    json!({"type":"array","items":items,"maxItems":maximum})
}
pub(crate) fn envelope(mut value: Value, id: &str) -> Value {
    value["$schema"] = json!("https://json-schema.org/draft/2020-12/schema");
    value["$id"] = json!(id);
    value["$comment"] = json!(
        "Production validation also enforces UTF-8 byte/item/node/depth ceilings, duplicate keys, query words/controls, public URLs/exclusions, exact caller references and current source provenance. No dispatch, acceptance or apply authority is conveyed."
    );
    value
}
pub fn schema() -> Value {
    let l = StageLimits::default();
    envelope(
        object(
            json!({"queries":array(string(600,false),l.max_queries),"urls":array(string(l.max_url_bytes,false),l.max_urls),"reason":string(l.max_text_bytes,false)}),
            &["queries", "urls", "reason"],
        ),
        "urn:lwiki:research-frontier:1",
    )
}

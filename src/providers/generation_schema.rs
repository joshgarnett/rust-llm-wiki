//! Version-one provider grammar projection. Local schema/semantic validation
//! remains authoritative; optional fields retain genuine omission semantics.
use crate::domain::{ErrorCode, Result, WikiError};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;

const MAX_NODES: usize = 4096;
const MAX_DEPTH: usize = 32;
const MAX_BYTES: usize = 65_536;
const MAX_OPTIONAL: usize = 4;
const MAX_VARIANTS: usize = 64;

fn unsupported(message: &str) -> WikiError {
    WikiError::invalid(format!("provider schema projection: {message}"))
}
fn ceiling() -> WikiError {
    WikiError::new(
        ErrorCode::BudgetExceeded,
        "provider schema projection ceiling",
    )
}

fn bounded(value: &Value) -> Result<()> {
    fn nodes(value: &Value, count: &mut usize, depth: usize) -> Result<()> {
        *count += 1;
        if *count > MAX_NODES || depth > MAX_DEPTH {
            return Err(ceiling());
        }
        match value {
            Value::Object(fields) => {
                for value in fields.values() {
                    nodes(value, count, depth + 1)?;
                }
            }
            Value::Array(values) => {
                for value in values {
                    nodes(value, count, depth + 1)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .filter(|n| *n <= MAX_BYTES)
                .ok_or_else(|| std::io::Error::other("schema byte ceiling"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    nodes(value, &mut 0, 0)?;
    serde_json::to_writer(Counter(0), value).map_err(|_| ceiling())
}

struct Projection<'a> {
    root: &'a Value,
    definitions: Map<String, Value>,
    active: BTreeSet<usize>,
    work: usize,
    variants: usize,
    copied_nodes: usize,
    copied_bytes: usize,
}

impl<'a> Projection<'a> {
    /// Charge all potentially amplified data before allocating its copy. Fixed
    /// grammar punctuation is separately bounded by work/variant limits and the
    /// final output check; caller strings and subtrees cannot expand first.
    fn charge_copy<T: serde::Serialize + ?Sized>(&mut self, value: &T, nodes: usize) -> Result<()> {
        struct Counter<'a>(&'a mut usize);
        impl std::io::Write for Counter<'_> {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                *self.0 = self
                    .0
                    .checked_add(bytes.len())
                    .filter(|n| *n <= MAX_BYTES)
                    .ok_or_else(|| std::io::Error::other("schema copy ceiling"))?;
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let next_nodes = self
            .copied_nodes
            .checked_add(nodes)
            .filter(|n| *n <= MAX_NODES)
            .ok_or_else(ceiling)?;
        let mut next_bytes = self.copied_bytes;
        serde_json::to_writer(Counter(&mut next_bytes), value).map_err(|_| ceiling())?;
        self.copied_nodes = next_nodes;
        self.copied_bytes = next_bytes;
        Ok(())
    }

    fn copy(&mut self, value: &Value) -> Result<Value> {
        fn count(value: &Value, remaining: &mut usize) -> Result<()> {
            *remaining = remaining.checked_sub(1).ok_or_else(ceiling)?;
            match value {
                Value::Object(fields) => {
                    for value in fields.values() {
                        count(value, remaining)?;
                    }
                }
                Value::Array(values) => {
                    for value in values {
                        count(value, remaining)?;
                    }
                }
                _ => {}
            }
            Ok(())
        }
        let mut remaining = MAX_NODES - self.copied_nodes;
        let initial = remaining;
        count(value, &mut remaining)?;
        self.charge_copy(value, initial - remaining)?;
        Ok(value.clone())
    }

    fn text(&mut self, value: &str) -> Result<String> {
        self.charge_copy(value, 1)?;
        Ok(value.to_owned())
    }

    fn required(&mut self, fields: &Map<String, Value>) -> Result<Value> {
        fields
            .keys()
            .map(|name| self.text(name).map(Value::String))
            .collect::<Result<Vec<_>>>()
            .map(Value::Array)
    }

    fn reference(&mut self, schema: Value) -> Result<Value> {
        let name = format!("p{}", self.definitions.len());
        let key = self.text(&name)?;
        self.definitions.insert(key, schema);
        let value = json!({"$ref":format!("#/$defs/{name}")});
        self.charge_copy(&value, 2)?;
        Ok(value)
    }

    fn run(&mut self, schema: &'a Value, depth: usize) -> Result<Value> {
        self.work += 1;
        if self.work > MAX_NODES || depth > MAX_DEPTH {
            return Err(ceiling());
        }
        let address = schema as *const Value as usize;
        if !self.active.insert(address) {
            return Err(unsupported("recursive references are unsupported"));
        }
        let result = self.node(schema, depth);
        self.active.remove(&address);
        result
    }

    fn node(&mut self, schema: &'a Value, depth: usize) -> Result<Value> {
        let fields = schema
            .as_object()
            .ok_or_else(|| unsupported("a concrete schema object is required"))?;
        // Removal of these assertions only widens the provider grammar. The
        // unchanged original validator still enforces them after decoding.
        const REMOVED: &[&str] = &[
            "$schema",
            "$id",
            "$comment",
            "$defs",
            "minItems",
            "maxItems",
            "uniqueItems",
            "minLength",
            "maxLength",
            "pattern",
            "format",
            "minimum",
            "maximum",
            "exclusiveMinimum",
            "exclusiveMaximum",
            "multipleOf",
            "minProperties",
            "maxProperties",
            "allOf",
            "not",
            "if",
            "then",
            "else",
            "dependentRequired",
        ];
        const STRUCTURAL: &[&str] = &[
            "$ref",
            "title",
            "description",
            "type",
            "enum",
            "const",
            "properties",
            "additionalProperties",
            "required",
            "items",
            "anyOf",
            "oneOf",
        ];
        if fields
            .keys()
            .any(|key| !REMOVED.contains(&key.as_str()) && !STRUCTURAL.contains(&key.as_str()))
        {
            return Err(unsupported("open, dynamic or unsupported schema shape"));
        }
        if let Some(reference) = fields.get("$ref") {
            if fields.keys().any(|key| {
                !REMOVED.contains(&key.as_str())
                    && !matches!(key.as_str(), "$ref" | "title" | "description")
            }) {
                return Err(unsupported("structural reference siblings are unsupported"));
            }
            let reference = reference
                .as_str()
                .filter(|value| value.starts_with("#/") && !value.contains('%'))
                .ok_or_else(|| unsupported("only local JSON-pointer references are supported"))?;
            let root = self.root;
            let target = root
                .pointer(&reference[1..])
                .ok_or_else(|| unsupported("local reference is missing"))?;
            return self.run(target, depth + 1);
        }
        if fields.contains_key("anyOf") || fields.contains_key("oneOf") {
            if fields.contains_key("anyOf") && fields.contains_key("oneOf")
                || ["type", "properties", "items", "enum", "const"]
                    .iter()
                    .any(|key| fields.contains_key(*key))
            {
                return Err(unsupported("intersected alternatives are unsupported"));
            }
            let alternatives = fields
                .get("anyOf")
                .or_else(|| fields.get("oneOf"))
                .and_then(Value::as_array)
                .filter(|values| !values.is_empty() && values.len() <= 16)
                .ok_or_else(|| unsupported("invalid or excessive alternatives"))?;
            let values = alternatives
                .iter()
                .map(|value| self.run(value, depth + 1))
                .collect::<Result<Vec<_>>>()?;
            return Ok(Value::Object(Map::from_iter([(
                "anyOf".into(),
                Value::Array(values),
            )])));
        }
        let mut output = Map::new();
        for key in ["title", "description"] {
            if let Some(value) = fields.get(key) {
                if !value.is_string() {
                    return Err(unsupported("annotation must be text"));
                }
                output.insert(key.into(), self.copy(value)?);
            }
        }
        let enumeration = if let Some(value) = fields.get("const") {
            Some(vec![self.copy(value)?])
        } else {
            fields
                .get("enum")
                .map(|value| {
                    value
                        .as_array()
                        .filter(|values| !values.is_empty())
                        .ok_or_else(|| unsupported("invalid enumeration"))?;
                    let Value::Array(values) = self.copy(value)? else {
                        unreachable!("checked array")
                    };
                    Ok::<_, WikiError>(values)
                })
                .transpose()?
        };
        let declared_type = fields
            .get("type")
            .map(|value| self.copy(value))
            .transpose()?
            .or_else(|| {
                enumeration.as_ref().and_then(|values| {
                    let mut types = BTreeSet::new();
                    for value in values {
                        types.insert(match value {
                            Value::Null => "null",
                            Value::Bool(_) => "boolean",
                            Value::String(_) => "string",
                            Value::Number(_) => "number",
                            _ => return None,
                        });
                    }
                    Some(if types.len() == 1 {
                        json!(types.into_iter().next().expect("nonempty enum"))
                    } else {
                        json!(types.into_iter().collect::<Vec<_>>())
                    })
                })
            });
        let declared_type =
            declared_type.ok_or_else(|| unsupported("an explicit concrete type is required"))?;
        let types = match &declared_type {
            Value::String(value) => vec![value.as_str()],
            Value::Array(values) => values
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .ok_or_else(|| unsupported("invalid type union"))
                })
                .collect::<Result<Vec<_>>>()?,
            _ => return Err(unsupported("invalid type")),
        };
        if types.is_empty()
            || types.iter().any(|kind| {
                ![
                    "object", "array", "string", "number", "integer", "boolean", "null",
                ]
                .contains(kind)
            })
            || types.iter().filter(|kind| **kind != "null").count() > 1
        {
            return Err(unsupported(
                "only a concrete type with optional null is supported",
            ));
        }
        if enumeration.as_ref().is_some_and(|values| {
            values
                .iter()
                .any(|value| value.is_object() || value.is_array())
        }) {
            return Err(unsupported("structured enumeration is unsupported"));
        }
        let object = types.contains(&"object");
        let array = types.contains(&"array");
        output.insert("type".into(), declared_type);
        if let Some(values) = enumeration {
            output.insert("enum".into(), values.into());
        }
        if object {
            if fields.get("additionalProperties") != Some(&Value::Bool(false)) {
                return Err(unsupported("objects must have closed properties"));
            }
            let properties = fields
                .get("properties")
                .and_then(Value::as_object)
                .ok_or_else(|| unsupported("object properties are required"))?;
            let mut required = BTreeSet::new();
            if let Some(value) = fields.get("required") {
                let names = value
                    .as_array()
                    .ok_or_else(|| unsupported("required must be an array"))?;
                for name in names {
                    let name = name
                        .as_str()
                        .ok_or_else(|| unsupported("invalid required name"))?;
                    if !properties.contains_key(name) || !required.insert(name) {
                        return Err(unsupported("duplicate or undeclared required name"));
                    }
                }
            }
            let optional = properties
                .keys()
                .filter(|name| !required.contains(name.as_str()))
                .map(String::as_str)
                .collect::<Vec<_>>();
            if optional.len() > MAX_OPTIONAL {
                return Err(ceiling());
            }
            let mut projected = Map::new();
            for (name, value) in properties {
                let value = self.run(value, depth + 1)?;
                projected.insert(
                    self.text(name)?,
                    if optional.is_empty() {
                        value
                    } else {
                        self.reference(value)?
                    },
                );
            }
            if optional.is_empty() {
                output.insert("required".into(), self.required(&projected)?);
                output.insert("properties".into(), projected.into());
                output.insert("additionalProperties".into(), Value::Bool(false));
            } else {
                let count = 1usize << optional.len();
                self.variants += count;
                if self.variants > MAX_VARIANTS {
                    return Err(ceiling());
                }
                let mut alternatives = Vec::new();
                for mask in 0..count {
                    let mut present = Map::new();
                    for (name, schema) in &projected {
                        if required.contains(name.as_str())
                            || optional.iter().enumerate().any(|(bit, optional)| {
                                *optional == name.as_str() && mask & (1usize << bit) != 0
                            })
                        {
                            present.insert(self.text(name)?, self.copy(schema)?);
                        }
                    }
                    let mut variant = Map::new();
                    for (name, value) in &output {
                        variant.insert(self.text(name)?, self.copy(value)?);
                    }
                    variant.insert("required".into(), self.required(&present)?);
                    variant.insert("properties".into(), present.into());
                    variant.insert("additionalProperties".into(), Value::Bool(false));
                    alternatives.push(Value::Object(variant));
                }
                return Ok(Value::Object(Map::from_iter([(
                    "anyOf".into(),
                    Value::Array(alternatives),
                )])));
            }
        } else if array {
            let items = fields
                .get("items")
                .ok_or_else(|| unsupported("array item schema is required"))?;
            output.insert("items".into(), self.run(items, depth + 1)?);
        }
        Ok(Value::Object(output))
    }
}

pub(super) fn project(schema: &Value) -> Result<Value> {
    bounded(schema)?;
    let mut projection = Projection {
        root: schema,
        definitions: Map::new(),
        active: BTreeSet::new(),
        work: 0,
        variants: 0,
        copied_nodes: 0,
        copied_bytes: 0,
    };
    let mut result = projection.run(schema, 0)?;
    if result.get("type").and_then(Value::as_str) != Some("object") || result.get("anyOf").is_some()
    {
        return Err(unsupported("root must be an all-required object"));
    }
    if !projection.definitions.is_empty() {
        result["$defs"] = projection.definitions.into();
    }
    bounded(&result)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::generation_wire::compile_schema;

    fn extraction() -> Value {
        serde_json::from_str(include_str!("../../schemas/extraction-v1.json")).unwrap()
    }
    fn valid_extraction() -> Value {
        json!({
            "schema":"lwiki.extraction.v1","packet_id":"packet_1",
            "packet_fingerprint":format!("blake3:{}", "0".repeat(64)),
            "mentions":[{"id":"m1","window_id":"w1","label":"Ada","type":"person","quote":"Ada"}],
            "assertions":[{"id":"a1","subject":"m1","predicate":"works_for",
                "object":{"kind":"mention","mention_id":"m1"},"negated":false,"modality":"asserted",
                "evidence":[{"window_id":"w1","stance":"supports","quote":"Ada"}]}],
            "unresolved":[]
        })
    }

    #[test]
    fn builtin_extraction_and_probe_project_to_strict_grammars() {
        for schema in [
            extraction(),
            json!({"type":"object","properties":{"ok":{"type":"boolean"}},
                "required":["ok"],"additionalProperties":false}),
        ] {
            compile_schema(&schema, false).unwrap();
            let projected = project(&schema).unwrap();
            compile_schema(&projected, true).unwrap();
            assert_eq!(project(&schema).unwrap(), projected);
        }
    }

    #[test]
    fn optional_presence_is_preserved_without_null_normalization() {
        let schema = extraction();
        let local = compile_schema(&schema, false).unwrap();
        let projected = compile_schema(&project(&schema).unwrap(), true).unwrap();
        let mut value = valid_extraction();
        assert!(local.is_valid(&value));
        assert!(projected.is_valid(&value));
        value["mentions"][0]["description"] = json!("A person");
        value["mentions"][0]["span"] = json!({"start":0,"end":3});
        value["assertions"][0]["valid_from"] = json!("2026-09-29");
        assert!(local.is_valid(&value));
        assert!(projected.is_valid(&value));
        value["mentions"][0]["description"] = Value::Null;
        assert!(!local.is_valid(&value));
        assert!(!projected.is_valid(&value));
    }

    #[test]
    fn relaxed_provider_constraints_still_fail_original_validation() {
        let schema = extraction();
        let local = compile_schema(&schema, false).unwrap();
        let projected = compile_schema(&project(&schema).unwrap(), true).unwrap();
        let mut span = valid_extraction();
        span["mentions"][0]["span"] = json!({"start":-1,"end":3});
        let mut date = valid_extraction();
        date["assertions"][0]["valid_until"] = json!("2026-02-30");
        let mut condition = valid_extraction();
        condition["assertions"][0]["predicate"] = json!("has_property");
        for value in [span, date, condition] {
            assert!(projected.is_valid(&value));
            assert!(!local.is_valid(&value));
        }
    }

    #[test]
    fn unsupported_shapes_and_branch_blowup_are_bounded() {
        for schema in [
            json!({"type":"object","properties":{},"required":[]}),
            json!({"type":"object","properties":{"x":{"type":"string"}},"additionalProperties":false}),
            json!({"type":"object","properties":{"x":{"$ref":"#/$defs/x"}},"required":["x"],"additionalProperties":false,"$defs":{"x":{"$ref":"#/$defs/x"}}}),
            json!({"type":"object","properties":{},"required":[],"additionalProperties":false,"patternProperties":{".*":{"type":"string"}}}),
        ] {
            assert!(project(&schema).is_err());
        }
        let optional = json!({"type":"object","properties":{"a":{"type":"string"},"b":{"type":"string"},"c":{"type":"string"},"d":{"type":"string"}},"additionalProperties":false});
        let mut fields = Map::new();
        for index in 0..5 {
            fields.insert(format!("n{index}"), optional.clone());
        }
        let schema = json!({"type":"object","required":fields.keys().collect::<Vec<_>>(),"properties":fields,"additionalProperties":false});
        assert_eq!(
            project(&schema).unwrap_err().code,
            ErrorCode::BudgetExceeded
        );
    }

    #[test]
    fn repeated_large_reference_stops_before_second_payload_copy() {
        for definition in [
            json!({"const":"x".repeat(48_000)}),
            json!({"enum":["x".repeat(48_000)]}),
        ] {
            let properties = (0..100)
                .map(|index| (format!("p{index:03}"), json!({"$ref":"#/$defs/large"})))
                .collect::<Map<_, _>>();
            let schema = json!({
                "type":"object", "required":properties.keys().collect::<Vec<_>>(),
                "properties":properties, "additionalProperties":false,
                "$defs":{"large":definition}
            });
            // The source is small; only reference expansion threatens the cap.
            bounded(&schema).unwrap();
            let mut projection = Projection {
                root: &schema,
                definitions: Map::new(),
                active: BTreeSet::new(),
                work: 0,
                variants: 0,
                copied_nodes: 0,
                copied_bytes: 0,
            };
            assert_eq!(
                projection.run(&schema, 0).unwrap_err().code,
                ErrorCode::BudgetExceeded
            );
            // Root + two reference/target pairs: the final-output bound was
            // never reached, and the other 98 references were never expanded.
            assert_eq!(projection.work, 5);
            assert!(projection.copied_bytes >= 48_000);
            assert!(projection.copied_bytes < MAX_BYTES);
            assert!(projection.copied_nodes < 16);
        }
    }
}

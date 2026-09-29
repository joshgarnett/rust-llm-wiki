//! Nonstreaming text generation with unchanged, offline local schema validation.
use super::{types::*, wire, wire_json};
use crate::{
    config::providers::{InstructionRole, OutputLimitField, ResponseMode, TrustedService},
    domain::*,
    jobs::TaskSpec,
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::Arc};

pub(super) fn compile_schema(schema: &Value, strict: bool) -> Result<Arc<jsonschema::Validator>> {
    // Meter every raw node before serialization or validator compilation.
    fn raw(v: &Value, n: &mut usize, d: usize) -> Result<()> {
        *n += 1;
        if *n > 4096 || d > 32 {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "schema structural ceiling",
            ));
        }
        match v {
            Value::Object(m) => {
                for v in m.values() {
                    raw(v, n, d + 1)?
                }
            }
            Value::Array(a) => {
                for v in a {
                    raw(v, n, d + 1)?
                }
            }
            _ => {}
        }
        Ok(())
    }
    raw(schema, &mut 0, 0)?;
    fn patterns(v: &Value, count: &mut usize, bytes: &mut usize) -> Result<()> {
        if let Some(m) = v.as_object() {
            let mut add = |s: &str| -> Result<()> {
                *count += 1;
                *bytes = bytes
                    .checked_add(s.len())
                    .ok_or_else(|| WikiError::invalid("schema pattern budget overflow"))?;
                if s.len() > 4096 || *count > 64 || *bytes > 16384 {
                    return Err(WikiError::new(
                        ErrorCode::BudgetExceeded,
                        "schema pattern work ceiling",
                    ));
                }
                Ok(())
            };
            if let Some(p) = m.get("pattern").and_then(Value::as_str) {
                add(p)?;
            }
            if let Some(p) = m.get("patternProperties").and_then(Value::as_object) {
                for key in p.keys() {
                    add(key)?;
                }
            }
            for v in m.values() {
                patterns(v, count, bytes)?;
            }
        } else if let Some(a) = v.as_array() {
            for v in a {
                patterns(v, count, bytes)?;
            }
        }
        Ok(())
    }
    patterns(schema, &mut 0, &mut 0)?;
    struct Cap(Vec<u8>);
    impl std::io::Write for Cap {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            if self.0.len().checked_add(b.len()).is_none_or(|n| n > 65536) {
                return Err(std::io::Error::other("schema ceiling"));
            }
            self.0.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(&mut Cap(Vec::new()), schema)
        .map_err(|_| WikiError::new(ErrorCode::BudgetExceeded, "schema byte ceiling"))?;
    fn walk<'a>(
        root: &'a Value,
        s: &'a Value,
        strict: bool,
        work: &mut usize,
        active: &mut BTreeSet<usize>,
        depth: usize,
    ) -> Result<()> {
        *work += 1;
        if *work > 4096 || depth > 32 {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "schema expansion ceiling",
            ));
        }
        let address = s as *const Value as usize;
        if !active.insert(address) {
            return Err(WikiError::invalid("recursive schema reference"));
        }
        let result = (|| {
            if s.is_boolean() {
                return if strict {
                    Err(WikiError::invalid(
                        "strict provider boolean schema unsupported",
                    ))
                } else {
                    Ok(())
                };
            }
            let m = s
                .as_object()
                .ok_or_else(|| WikiError::invalid("schema object required"))?;
            if m.contains_key("$id") && !std::ptr::eq(root, s) {
                return Err(WikiError::invalid("nested schema identity unsupported"));
            }
            let local = [
                "$schema",
                "$id",
                "$comment",
                "$defs",
                "$ref",
                "title",
                "description",
                "type",
                "enum",
                "const",
                "properties",
                "patternProperties",
                "additionalProperties",
                "required",
                "items",
                "prefixItems",
                "contains",
                "minContains",
                "maxContains",
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
                "propertyNames",
                "dependentRequired",
                "dependentSchemas",
                "allOf",
                "anyOf",
                "oneOf",
                "not",
                "if",
                "then",
                "else",
            ];
            let provider = [
                "$schema",
                "$defs",
                "$ref",
                "title",
                "description",
                "type",
                "enum",
                "properties",
                "additionalProperties",
                "required",
                "items",
                "anyOf",
            ];
            for key in m.keys() {
                if !local.contains(&key.as_str()) || (strict && !provider.contains(&key.as_str())) {
                    return Err(WikiError::invalid("unsupported schema keyword"));
                }
            }
            if let Some(uri) = m.get("$schema")
                && uri.as_str() != Some("https://json-schema.org/draft/2020-12/schema")
            {
                return Err(WikiError::invalid("unsupported schema draft"));
            }
            if let Some(f) = m.get("format")
                && f.as_str() != Some("date")
            {
                return Err(WikiError::invalid("unsupported schema format"));
            }
            if let Some(reference) = m.get("$ref") {
                let r = reference
                    .as_str()
                    .filter(|r| r.starts_with("#/"))
                    .ok_or_else(|| WikiError::invalid("nonlocal schema reference"))?;
                // Canonical JSON pointer only; percent-decoded URI fragments are not silently reinterpreted.
                if r.contains('%') {
                    return Err(WikiError::invalid("unsupported schema reference encoding"));
                }
                let target = root
                    .pointer(&r[1..])
                    .ok_or_else(|| WikiError::invalid("schema reference missing"))?;
                walk(root, target, strict, work, active, depth + 1)?;
            }
            if strict
                && (m.get("type").is_some_and(|t| {
                    t == "object"
                        || t.as_array()
                            .is_some_and(|a| a.iter().any(|t| t == "object"))
                }) || m.contains_key("properties"))
            {
                if m.get("additionalProperties") != Some(&Value::Bool(false)) {
                    return Err(WikiError::invalid(
                        "strict objects require closed properties",
                    ));
                }
                let properties = m
                    .get("properties")
                    .and_then(Value::as_object)
                    .ok_or_else(|| WikiError::invalid("strict object properties required"))?;
                let required = m
                    .get("required")
                    .and_then(Value::as_array)
                    .ok_or_else(|| WikiError::invalid("strict object required fields missing"))?;
                let names = required
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<BTreeSet<_>>();
                if names.len() != required.len()
                    || names.len() != properties.len()
                    || !properties.keys().all(|k| names.contains(k.as_str()))
                {
                    return Err(WikiError::invalid(
                        "strict object fields must all be required",
                    ));
                }
            }
            for key in [
                "$defs",
                "properties",
                "patternProperties",
                "dependentSchemas",
            ] {
                if let Some(values) = m.get(key) {
                    for v in values
                        .as_object()
                        .ok_or_else(|| WikiError::invalid("schema map malformed"))?
                        .values()
                    {
                        walk(root, v, strict, work, active, depth + 1)?;
                    }
                }
            }
            for key in ["allOf", "anyOf", "oneOf", "prefixItems"] {
                if let Some(values) = m.get(key) {
                    for v in values
                        .as_array()
                        .ok_or_else(|| WikiError::invalid("schema list malformed"))?
                    {
                        walk(root, v, strict, work, active, depth + 1)?;
                    }
                }
            }
            for key in [
                "items",
                "contains",
                "additionalProperties",
                "propertyNames",
                "not",
                "if",
                "then",
                "else",
            ] {
                if let Some(v) = m.get(key) {
                    if key == "additionalProperties" && v.is_boolean() {
                        continue;
                    }
                    walk(root, v, strict, work, active, depth + 1)?;
                }
            }
            Ok(())
        })();
        active.remove(&address);
        result
    }
    if strict
        && (schema.get("type").and_then(Value::as_str) != Some("object")
            || schema.get("anyOf").is_some())
    {
        return Err(WikiError::invalid("strict provider root must be object"));
    }
    walk(schema, schema, strict, &mut 0, &mut BTreeSet::new(), 0)?;
    jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .offline()
        .should_validate_formats(true)
        .with_pattern_options(
            jsonschema::PatternOptions::regex()
                .size_limit(65536)
                .dfa_size_limit(65536),
        )
        .build(schema)
        .map(Arc::new)
        .map_err(|_| WikiError::invalid("invalid or unsupported local schema"))
}

pub(super) fn prepare(
    service: &TrustedService,
    task: &TaskSpec,
    input: &RemoteInput,
    purpose: DispatchPurpose,
) -> Result<PreparedWire> {
    wire::verify_task(service, task, input)?;
    let RemoteOperation::Generate {
        instructions,
        data,
        output_schema,
        max_output_tokens,
    } = &input.operation
    else {
        return Err(WikiError::invalid("generation operation required"));
    };
    let s = service.service();
    if input.version != 1
        || instructions.is_empty()
        || *max_output_tokens == 0
        || *max_output_tokens > s.max_output_tokens.unwrap_or(4096)
    {
        return Err(WikiError::invalid("generation options invalid"));
    }
    let model = s
        .model
        .as_deref()
        .ok_or_else(|| WikiError::invalid("generation model required"))?;
    let instruction_role = s.instruction_role.unwrap_or(InstructionRole::System);
    let output_limit_field = s
        .output_limit_field
        .unwrap_or(OutputLimitField::MaxCompletionTokens);
    let response_mode = s.response_mode.unwrap_or(ResponseMode::TextJson);
    let basis = if s.ca_bytes.is_none()
        && s.url == "https://api.openai.com/v1/chat/completions"
        && model == "gpt-4.1-2025-04-14"
        && matches!(output_limit_field, OutputLimitField::MaxCompletionTokens)
    {
        if *max_output_tokens > 32768 {
            return Err(WikiError::invalid("documented completion ceiling exceeded"));
        }
        BoundBasis::OpenAiChatGpt41SnapshotV1
    } else {
        BoundBasis::UnknownCompatible
    };
    let validator = compile_schema(
        output_schema,
        matches!(response_mode, ResponseMode::JsonSchema),
    )?;
    let mut body = json!({"model":model,"messages":[{"role":match instruction_role{InstructionRole::System=>"system",InstructionRole::Developer=>"developer"},"content":instructions},{"role":"user","content":data}],"stream":false,"n":1});
    body[match output_limit_field {
        OutputLimitField::MaxCompletionTokens => "max_completion_tokens",
        OutputLimitField::MaxTokens => "max_tokens",
    }] = (*max_output_tokens).into();
    if matches!(response_mode, ResponseMode::JsonSchema) {
        body["response_format"] = json!({"type":"json_schema","json_schema":{"name":"lwiki_output","strict":true,"schema":output_schema}});
    }
    let schema_fingerprint =
        Blake3Hash::digest(crate::graph::packet::canonical_json(output_schema)?);
    wire::seal(
        service,
        task,
        input,
        purpose,
        ServiceRole::Generate,
        wire::json_body(service, &body)?,
        WireContract::Generation(GenerationContract {
            basis,
            instruction_role,
            output_limit_field,
            response_mode,
            total_generated_token_limit: *max_output_tokens,
            schema_fingerprint,
            schema: Arc::new(output_schema.clone()),
            validator,
        }),
    )
}
pub(super) fn observe(p: &PreparedWire, r: &TransportReply) -> ObservedUsage {
    wire_json::observe(p, r, true)
}
pub(super) fn decode(p: &PreparedWire, r: &TransportReply) -> Result<ValidatedOutput> {
    if !(200..300).contains(&r.status) {
        return Err(WikiError::invalid("unsuccessful generation response"));
    }
    let WireContract::Generation(c) = &p.contract else {
        return Err(WikiError::invalid("generation contract required"));
    };
    let RemoteOperation::Generate {
        output_schema,
        max_output_tokens,
        ..
    } = &p.input.operation
    else {
        return Err(WikiError::invalid("retained generation input required"));
    };
    if c.schema.as_ref() != output_schema
        || c.total_generated_token_limit != *max_output_tokens
        || c.schema_fingerprint
            != Blake3Hash::digest(crate::graph::packet::canonical_json(output_schema)?)
    {
        return Err(WikiError::invalid(
            "retained generation schema or options differ",
        ));
    }
    let v = wire_json::reply_json(p, r)?;
    let model = wire_json::returned_model(p, &v)?;
    if !wire_json::usage_valid(&v, true)
        || !wire_json::supported_usage(p, &v, true)
        || !wire_json::totals_within_contract(p, &v)
    {
        return Err(WikiError::invalid("generation usage malformed"));
    }
    let envelope = v
        .as_object()
        .ok_or_else(|| WikiError::invalid("generation envelope malformed"))?;
    for (key, value) in envelope {
        let valid = match key.as_str() {
            "model" | "choices" | "usage" => true,
            "id" => wire_json::field(&v, "id", 128).is_some(),
            "object" => value.as_str() == Some("chat.completion"),
            "created" => value.as_u64().is_some(),
            "system_fingerprint" | "service_tier" => {
                value.is_null() || wire_json::field(&v, key, 256).is_some()
            }
            _ => false,
        };
        if !valid {
            return Err(WikiError::invalid("generation envelope extension invalid"));
        }
    }
    let choices = v
        .get("choices")
        .and_then(Value::as_array)
        .filter(|a| a.len() == 1)
        .ok_or_else(|| WikiError::invalid("one generation choice required"))?;
    let choice = &choices[0];
    if choice.as_object().is_none_or(|m| {
        m.iter().any(|(k, v)| {
            !matches!(k.as_str(), "index" | "message" | "finish_reason")
                && !(k == "logprobs" && v.is_null())
        })
    }) {
        return Err(WikiError::invalid("generation choice extension invalid"));
    }
    if choice.get("index").and_then(Value::as_u64) != Some(0)
        || choice.get("finish_reason").and_then(Value::as_str) != Some("stop")
    {
        return Err(WikiError::invalid(
            "generation incomplete or choice invalid",
        ));
    }
    let message = choice
        .get("message")
        .and_then(Value::as_object)
        .ok_or_else(|| WikiError::invalid("generation message missing"))?;
    for (key, value) in message {
        match key.as_str() {
            "role" | "content" => {}
            "refusal" | "function_call" if value.is_null() => {}
            "tool_calls" if value.is_null() || value.as_array().is_some_and(Vec::is_empty) => {}
            _ => {
                return Err(WikiError::invalid(
                    "generation refusal, tools or extension present",
                ));
            }
        }
    }
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return Err(WikiError::invalid("generation role invalid"));
    }
    let content = message
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| WikiError::invalid("generation text missing"))?;
    let value = wire_json::parse(
        content.as_bytes(),
        p.bound.response_bytes as usize,
        65536,
        32,
    )?;
    if !c.validator.is_valid(&value) {
        return Err(WikiError::invalid(
            "generation output violates local schema",
        ));
    }
    if matches!(p.purpose, DispatchPurpose::Probe { .. }) {
        Ok(ValidatedOutput::Probe {
            role: ServiceRole::Generate,
        })
    } else {
        Ok(ValidatedOutput::Generation {
            value,
            text: content.to_owned(),
            returned_model: Some(model),
        })
    }
}

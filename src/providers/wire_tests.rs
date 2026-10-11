use crate as library;
use crate::test_paths;
#[allow(dead_code)]
#[path = "../../tests/fixtures/p16c/common.rs"]
pub(crate) mod common;
use super::{embedding_wire as embed, generation_wire as genwire, types::*, wire_json};
use crate::jobs::*;
use common::*;
use serde_json::{Value, json};

fn embedding_input() -> RemoteInput {
    RemoteInput {
        version: 1,
        operation: RemoteOperation::Embed {
            inputs: vec![
                EmbeddingInput {
                    input_hash: hash("é"),
                    utf8: "é".into(),
                },
                EmbeddingInput {
                    input_hash: hash("é"),
                    utf8: "é".into(),
                },
            ],
            expected_dimensions: Some(2),
            representation_fingerprint: hash("render.v1"),
        },
    }
}
fn generation_input() -> RemoteInput {
    RemoteInput {
        version: 1,
        operation: RemoteOperation::Generate {
            instructions: "Return data".into(),
            data: "untrusted data".into(),
            output_schema: json!({"type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"],"additionalProperties":false}),
            max_output_tokens: 16,
        },
    }
}
fn reply(v: &Value) -> TransportReply {
    TransportReply::new(200, vec![], serde_json::to_vec(v).unwrap()).unwrap()
}
fn chat() -> Value {
    json!({"model":"test-model","choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"{\"ok\":true}"}}],"usage":{"prompt_tokens":10,"completion_tokens":8,"total_tokens":18,"prompt_tokens_details":{"cached_tokens":3},"completion_tokens_details":{"reasoning_tokens":2}}})
}
fn responses_case(model: &str, extra: &str) -> Case {
    case_with_adapter(
        ServiceRole::Generate,
        generation_input(),
        model,
        "https://gateway.example/v1/responses",
        extra,
        |_| {},
        "responses-v1",
    )
}
fn gateway_fixture(name: &str) -> Value {
    serde_json::from_str(match name {
        "gpt" => include_str!("../../tests/fixtures/gateway/responses_gpt.json"),
        "claude" => include_str!("../../tests/fixtures/gateway/responses_claude.json"),
        "incomplete" => include_str!("../../tests/fixtures/gateway/responses_incomplete.json"),
        _ => panic!("unknown fixture"),
    })
    .unwrap()
}

#[test]
fn gateway_embedding_usage_extensions_preserve_full_vector_decode() {
    for (model, dimensions) in [("titan-embed-v2", 1024), ("gemini-embedding-001", 3072)] {
        let mut input = embedding_input();
        let RemoteOperation::Embed {
            inputs,
            expected_dimensions,
            ..
        } = &mut input.operation
        else {
            panic!("embedding input expected");
        };
        inputs.truncate(1);
        *expected_dimensions = Some(dimensions);
        let c = case(
            ServiceRole::Embed,
            input.clone(),
            model,
            "https://gateway.example/v1/embeddings",
            "",
            |_| {},
        );
        let p =
            embed::prepare(&c.trusted, &c.spec.tasks[0], &input, DispatchPurpose::Task).unwrap();
        let mut vector = vec![0.0f32; dimensions as usize];
        vector[0] = 1.0;
        let response = json!({"object":"list","model":model,"data":[{"index":0,"embedding":vector}],"usage":{"completion_tokens":0,"prompt_tokens":6,"total_tokens":6,"completion_tokens_details":null,"prompt_tokens_details":null}});
        let ValidatedOutput::Embeddings { vectors, .. } =
            embed::decode(&p, &reply(&response)).unwrap()
        else {
            panic!("embeddings expected")
        };
        assert_eq!(vectors.len(), 1);
        assert_eq!(vectors[0].len(), dimensions as usize);
        assert!(
            embed::observe(&p, &reply(&response))
                .contract_violation
                .is_none()
        );
    }
}

#[test]
fn responses_v1_encodes_no_tools_or_storage_and_decodes_gateway_samples() {
    for (name, model) in [("gpt", "gpt-6-luna"), ("claude", "claude-4.5-haiku")] {
        let c = responses_case(model, "response_mode=\"json-schema\"\n");
        let p = genwire::prepare(
            &c.trusted,
            &c.spec.tasks[0],
            &generation_input(),
            DispatchPurpose::Task,
        )
        .unwrap();
        let request: Value = serde_json::from_slice(&p.body).unwrap();
        assert_eq!(request["model"], model);
        assert_eq!(request["instructions"], "Return data");
        assert_eq!(
            request["input"],
            json!([{"role":"user","content":"untrusted data"}])
        );
        assert_eq!(request["stream"], false);
        assert_eq!(request["store"], false);
        assert_eq!(request["max_output_tokens"], 16);
        assert_eq!(request["text"]["format"]["type"], "json_schema");
        for key in [
            "tools",
            "tool_choice",
            "previous_response_id",
            "messages",
            "n",
            "response_format",
        ] {
            assert!(request.get(key).is_none(), "{key}");
        }
        let fixture = gateway_fixture(name);
        let ValidatedOutput::Generation {
            value,
            text,
            returned_model,
        } = genwire::decode(&p, &reply(&fixture)).unwrap()
        else {
            panic!("generation expected")
        };
        assert_eq!(value, json!({"ok":true}));
        assert_eq!(text, "{\"ok\":true}");
        assert_eq!(returned_model.as_deref(), Some(model));
        assert!(
            genwire::observe(&p, &reply(&fixture))
                .contract_violation
                .is_none()
        );
    }
}

#[test]
fn responses_incomplete_refusal_and_tools_are_unsuccessful_but_observed() {
    let c = responses_case("gpt-6-luna", "");
    let p = genwire::prepare(
        &c.trusted,
        &c.spec.tasks[0],
        &generation_input(),
        DispatchPurpose::Task,
    )
    .unwrap();
    let incomplete = gateway_fixture("incomplete");
    let error = genwire::decode(&p, &reply(&incomplete)).err().unwrap();
    assert_eq!(error.details["reason"], "incomplete_max_output_tokens");
    let observed = genwire::observe(&p, &reply(&incomplete));
    assert!(matches!(observed.usage, KnownOrUnknown::Known(_)));
    let mut response = gateway_fixture("gpt");
    for (item, reason) in [
        (
            json!({"type":"function_call","name":"unsafe"}),
            "tool_output",
        ),
        (json!({"type":"alien"}), "unknown_output_item"),
    ] {
        response["output"].as_array_mut().unwrap().push(item);
        assert_eq!(
            genwire::decode(&p, &reply(&response))
                .err()
                .unwrap()
                .details["reason"],
            reason
        );
        response["output"].as_array_mut().unwrap().pop();
    }
    response["output"][1]["content"] = json!([{"type":"refusal","refusal":"no"}]);
    assert_eq!(
        genwire::decode(&p, &reply(&response))
            .err()
            .unwrap()
            .details["reason"],
        "refusal"
    );
    let mut split = gateway_fixture("gpt");
    split["output"][1]["content"] = json!([
        {"type":"output_text","text":"{\"ok\":"},
        {"type":"output_text","text":"true}"}
    ]);
    assert!(genwire::decode(&p, &reply(&split)).is_ok());
    let second = split["output"][1].clone();
    split["output"].as_array_mut().unwrap().push(second);
    assert_eq!(
        genwire::decode(&p, &reply(&split)).err().unwrap().details["reason"],
        "assistant_message_count"
    );
    let mut failed = gateway_fixture("gpt");
    failed["status"] = "failed".into();
    assert_eq!(
        genwire::decode(&p, &reply(&failed)).err().unwrap().details["reason"],
        "failed"
    );
    assert!(matches!(
        genwire::observe(&p, &reply(&failed)).usage,
        KnownOrUnknown::Known(_)
    ));
}

#[test]
fn gateway_chat_reasoning_metadata_and_usage_extensions_do_not_replace_text() {
    for (model, usage, content) in [
        (
            "claude-4.5-haiku",
            json!({"completion_tokens":17,"prompt_tokens":22,"total_tokens":39,"completion_tokens_details":{"reasoning_tokens":0,"text_tokens":17},"prompt_tokens_details":{"cached_tokens":0,"text_tokens":22,"cache_write_tokens":0,"cache_creation_tokens":0},"cache_creation_input_tokens":0,"cache_read_input_tokens":0}),
            "```json\n{\"ok\":true}\n```",
        ),
        (
            "gpt-6-luna",
            json!({"completion_tokens":32,"prompt_tokens":24,"total_tokens":56,"completion_tokens_details":{"reasoning_tokens":21},"prompt_tokens_details":{"cached_tokens":0,"cache_write_tokens":0,"cache_creation_tokens":0}}),
            "{\"ok\":true}",
        ),
    ] {
        let c = case(
            ServiceRole::Generate,
            generation_input(),
            model,
            "https://gateway.example/v1/chat/completions",
            "",
            |_| {},
        );
        let p = genwire::prepare(
            &c.trusted,
            &c.spec.tasks[0],
            &generation_input(),
            DispatchPurpose::Task,
        )
        .unwrap();
        let mut response = chat();
        response["model"] = model.into();
        response["usage"] = usage;
        response["choices"][0]["message"]["content"] = content.into();
        response["choices"][0]["message"]["reasoning_content"] = "not output".into();
        response["choices"][0]["message"]["reasoning_items"] = json!([{"text":"also not output"}]);
        let ValidatedOutput::Generation { value, text, .. } =
            genwire::decode(&p, &reply(&response)).unwrap()
        else {
            panic!("generation expected")
        };
        assert_eq!(value, json!({"ok":true}));
        assert_eq!(text, "{\"ok\":true}");
        assert!(
            genwire::observe(&p, &reply(&response))
                .contract_violation
                .is_none()
        );
    }
}

#[test]
fn reordered_vectors_valid_missing_nan_dimension_batch_rejected() {
    let c = case(
        ServiceRole::Embed,
        embedding_input(),
        "test-model",
        "https://gateway.example/embeddings",
        "",
        |_| {},
    );
    let input: RemoteInput = serde_json::from_slice(
        &std::fs::read(c.fs.root().path().join("inputs/task.json")).unwrap(),
    )
    .unwrap();
    let p = embed::prepare(&c.trusted, &c.spec.tasks[0], &input, DispatchPurpose::Task).unwrap();
    let request: Value = serde_json::from_slice(&p.body).unwrap();
    assert_eq!(request["input"], json!(["é", "é"]));
    assert!(p.body.len() > std::str::from_utf8(&p.body).unwrap().chars().count());
    assert_eq!(p.bound.request_bytes, p.body.len() as u64);
    assert!(request.get("dimensions").is_none());
    let good = json!({"model":"test-model","object":"list","data":[{"index":1,"embedding":[3,4]},{"index":0,"embedding":[1,2]}]});
    let ValidatedOutput::Embeddings { vectors, .. } = embed::decode(&p, &reply(&good)).unwrap()
    else {
        panic!()
    };
    assert_eq!(vectors, vec![vec![1., 2.], vec![3., 4.]]);
    let mut bads = Vec::new();
    let mut v = good.clone();
    v["data"][1]["index"] = 1.into();
    bads.push(v);
    let mut v = good.clone();
    v["data"][0]["index"] = u64::MAX.into();
    bads.push(v);
    let mut v = good.clone();
    v["data"] = json!([good["data"][0].clone()]);
    bads.push(v);
    for coordinate in [Value::Null, json!("NaN"), json!(1e100), json!(1e-100)] {
        let mut v = good.clone();
        v["data"][0]["embedding"] = json!([coordinate, coordinate]);
        bads.push(v);
    }
    let mut v = good.clone();
    v["data"][0]["embedding"] = json!([1]);
    bads.push(v);
    for v in bads {
        assert!(embed::decode(&p, &reply(&v)).is_err());
    }
    for bytes in [
        br#"{"model":"a","model":"b","data":[]}"#.as_slice(),
        br#"{"data":[{"index":0,"index":0}]}"#,
        br#"{"data":[{"embedding":[1e999]}]}"#,
    ] {
        assert!(wire_json::parse(bytes, 8192, 100, 8).is_err());
    }
    assert_eq!(c.inputs.0.load(std::sync::atomic::Ordering::SeqCst), 0);
    for (extra, valid) in [("dimensions=2\n", true), ("dimensions=3\n", false)] {
        let c = case(
            ServiceRole::Embed,
            embedding_input(),
            "test-model",
            "https://gateway.example/embeddings",
            extra,
            |_| {},
        );
        let result = embed::prepare(
            &c.trusted,
            &c.spec.tasks[0],
            &embedding_input(),
            DispatchPurpose::Task,
        );
        if valid {
            let p = result.unwrap();
            let body: Value = serde_json::from_slice(&p.body).unwrap();
            assert_eq!(body["dimensions"], 2);
            assert!(embed::decode(&p, &reply(&good)).is_ok());
        } else {
            assert!(result.is_err());
        }
    }
    let c = case(
        ServiceRole::Embed,
        embedding_input(),
        "text-embedding-3-small",
        "https://api.openai.com/v1/embeddings",
        "dimensions=1537\n",
        |_| {},
    );
    assert!(
        embed::prepare(
            &c.trusted,
            &c.spec.tasks[0],
            &embedding_input(),
            DispatchPurpose::Task
        )
        .is_err()
    );
}

#[test]
fn refusal_truncation_tools_malformed_generation_receipt_no_repair() {
    let c = case(
        ServiceRole::Generate,
        generation_input(),
        "test-model",
        "https://gateway.example/chat",
        "",
        |_| {},
    );
    let input = generation_input();
    let p = genwire::prepare(&c.trusted, &c.spec.tasks[0], &input, DispatchPurpose::Task).unwrap();
    assert!(genwire::decode(&p, &reply(&chat())).is_ok());
    let mut fenced = chat();
    fenced["choices"][0]["message"]["content"] = "```json\n{\"ok\":true}\n```".into();
    assert!(genwire::decode(&p, &reply(&fenced)).is_ok());
    let mut reasoning = chat();
    reasoning["choices"][0]["message"]["reasoning_content"] = "private reasoning".into();
    reasoning["choices"][0]["message"]["reasoning_items"] = json!([{"opaque":"ignored"}]);
    assert!(genwire::decode(&p, &reply(&reasoning)).is_ok());
    for content in [
        "prose ```json\n{\"ok\":true}\n```",
        "```json\n{\"ok\":true}\n``` trailing",
        "prose {\"ok\":true}",
        "{\"ok\":true} false",
        "{\"ok\":false,\"ok\":true}",
        "{\"ok\":\"true\"}",
    ] {
        let mut v = chat();
        v["choices"][0]["message"]["content"] = content.into();
        assert!(genwire::decode(&p, &reply(&v)).is_err());
    }
    for reason in ["length", "content_filter", "tool_calls", "unknown"] {
        let mut v = chat();
        v["choices"][0]["finish_reason"] = reason.into();
        assert!(genwire::decode(&p, &reply(&v)).is_err());
    }
    for (key, value) in [
        ("refusal", json!("no")),
        ("tool_calls", json!([{}])),
        ("tool_calls", json!({})),
        ("function_call", json!({})),
        ("content", Value::Null),
        ("role", json!("user")),
        ("audio", json!({})),
    ] {
        let mut v = chat();
        v["choices"][0]["message"][key] = value;
        assert!(genwire::decode(&p, &reply(&v)).is_err());
    }
    for (key, value) in [
        ("refusal", Value::Null),
        ("tool_calls", Value::Null),
        ("tool_calls", json!([])),
        ("function_call", Value::Null),
    ] {
        let mut v = chat();
        v["choices"][0]["message"][key] = value;
        assert!(genwire::decode(&p, &reply(&v)).is_ok());
    }
    let mut v = chat();
    v["choices"][0]["index"] = 1.into();
    assert!(genwire::decode(&p, &reply(&v)).is_err());
    let mut v = chat();
    let second = v["choices"][0].clone();
    v["choices"].as_array_mut().unwrap().push(second);
    assert!(genwire::decode(&p, &reply(&v)).is_err());
}

#[test]
fn local_schema_offline_constraints_regex_and_reference_work_are_preserved() {
    let schema: Value =
        serde_json::from_str(include_str!("../../schemas/extraction-v1.json")).unwrap();
    assert!(genwire::compile_schema(&schema, false).is_ok());
    assert!(genwire::compile_schema(&schema, true).is_err());
    for schema in [
        json!({"$ref":"https://example.invalid/schema"}),
        json!({"$ref":"file:///tmp/schema"}),
        json!({"$defs":{"x":{"$ref":"#/$defs/x"}},"$ref":"#/$defs/x"}),
        json!({"type":"string","pattern":"(?=x)x"}),
        json!({"type":"string","pattern":"(x)\\1"}),
        json!({"type":"string","format":"custom"}),
    ] {
        assert!(genwire::compile_schema(&schema, false).is_err());
    }
    let date = genwire::compile_schema(&json!({"type":"string","format":"date"}), false).unwrap();
    assert!(date.is_valid(&json!("2024-02-29")));
    assert!(!date.is_valid(&json!("2023-02-29")));
    let local = genwire::compile_schema(
        &json!({"$defs":{"x":{"type":"integer","minimum":3}},"$ref":"#/$defs/x"}),
        false,
    )
    .unwrap();
    assert!(!local.is_valid(&json!(2)));
    assert!(local.is_valid(&json!(3)));
    let mut defs = serde_json::Map::new();
    defs.insert("x0".into(), json!({"type":"string"}));
    for n in 1..15 {
        defs.insert(format!("x{n}"),json!({"allOf":[{"$ref":format!("#/$defs/x{}",n-1)},{"$ref":format!("#/$defs/x{}",n-1)}]}));
    }
    assert!(genwire::compile_schema(&json!({"$defs":defs,"$ref":"#/$defs/x14"}), false).is_err());
    assert!(wire_json::parse(b"[1,2,3]", 32, 3, 2).is_err());
    assert!(wire_json::parse(b"[[[0]]]", 32, 10, 2).is_err());
}

#[test]
fn curated_identity_bounds_and_options_cannot_be_inferred_or_forged() {
    let c = case(
        ServiceRole::Embed,
        embedding_input(),
        "text-embedding-3-small",
        "https://api.openai.com/v1/embeddings",
        "",
        |_| {},
    );
    let p = embed::prepare(
        &c.trusted,
        &c.spec.tasks[0],
        &embedding_input(),
        DispatchPurpose::Task,
    )
    .unwrap();
    assert!(matches!(
        p.bound.billable_bounds[&BillableClass::Input],
        TokenBound::ProvenUpper { count: 16384, .. }
    ));
    let mut many = embedding_input();
    if let RemoteOperation::Embed { inputs, .. } = &mut many.operation {
        let item = inputs[0].clone();
        *inputs = vec![item; 37];
    }
    let c = case(
        ServiceRole::Embed,
        many.clone(),
        "text-embedding-3-small",
        "https://api.openai.com/v1/embeddings",
        "max_batch_items=37\n",
        |_| {},
    );
    let p = embed::prepare(&c.trusted, &c.spec.tasks[0], &many, DispatchPurpose::Task).unwrap();
    assert!(matches!(
        p.bound.billable_bounds[&BillableClass::Input],
        TokenBound::ProvenUpper { count: 300000, .. }
    ));
    let c = case(
        ServiceRole::Embed,
        embedding_input(),
        "text-embedding-3-small",
        "https://gateway.example/embeddings",
        "tokenizer=\"cl100k_base\"\n",
        |_| {},
    );
    let p = embed::prepare(
        &c.trusted,
        &c.spec.tasks[0],
        &embedding_input(),
        DispatchPurpose::Task,
    )
    .unwrap();
    assert_eq!(
        p.bound.billable_bounds[&BillableClass::Input],
        TokenBound::Unknown
    );
    let ca = test_paths::fixture(env!("CARGO_MANIFEST_DIR"), "tests/fixtures/p16b/ca.pem");
    let c = case(
        ServiceRole::Embed,
        embedding_input(),
        "text-embedding-3-small",
        "https://api.openai.com/v1/embeddings",
        &format!("ca_file={}\n", quote(ca.to_str().unwrap())),
        |_| {},
    );
    let p = embed::prepare(
        &c.trusted,
        &c.spec.tasks[0],
        &embedding_input(),
        DispatchPurpose::Task,
    )
    .unwrap();
    assert_eq!(
        p.bound.billable_bounds[&BillableClass::Input],
        TokenBound::Unknown
    );
    let c = case(
        ServiceRole::Embed,
        embedding_input(),
        "text-embedding-3-small",
        "https://api.openai.com/v1/embeddings",
        "max_input_tokens=8191\n",
        |_| {},
    );
    assert!(
        embed::prepare(
            &c.trusted,
            &c.spec.tasks[0],
            &embedding_input(),
            DispatchPurpose::Task
        )
        .is_err()
    );
    for extra in [
        "instruction_role=\"developer\"\noutput_limit_field=\"max_tokens\"\nresponse_mode=\"text-json\"\n",
        "instruction_role=\"system\"\noutput_limit_field=\"max_completion_tokens\"\nresponse_mode=\"json-schema\"\n",
    ] {
        let c = case(
            ServiceRole::Generate,
            generation_input(),
            "test-model",
            "https://gateway.example/chat",
            extra,
            |_| {},
        );
        let p = genwire::prepare(
            &c.trusted,
            &c.spec.tasks[0],
            &generation_input(),
            DispatchPurpose::Task,
        )
        .unwrap();
        let body: Value = serde_json::from_slice(&p.body).unwrap();
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(
            body.get("max_tokens").is_some(),
            extra.contains("=\"max_tokens\"")
        );
        assert_eq!(
            body.get("response_format").is_some(),
            extra.contains("json-schema")
        );
        assert_eq!(p.bound.request_bytes, p.body.len() as u64);
        let mut task = c.spec.tasks[0].clone();
        task.settings_hash = hash("forged");
        assert!(
            genwire::prepare(
                &c.trusted,
                &task,
                &generation_input(),
                DispatchPurpose::Task
            )
            .is_err()
        );
    }
    let c = case(
        ServiceRole::Generate,
        generation_input(),
        "gpt-4.1-2025-04-14",
        "https://api.openai.com/v1/chat/completions",
        "",
        |_| {},
    );
    let p = genwire::prepare(
        &c.trusted,
        &c.spec.tasks[0],
        &generation_input(),
        DispatchPurpose::Task,
    )
    .unwrap();
    assert!(matches!(
        p.bound.billable_bounds[&BillableClass::Output],
        TokenBound::ProvenUpper { count: 16, .. }
    ));
    let mut changed = generation_input();
    if let RemoteOperation::Generate {
        max_output_tokens, ..
    } = &mut changed.operation
    {
        *max_output_tokens = 8;
    }
    let mut task = c.spec.tasks[0].clone();
    let fp = super::wire::task_fingerprints(&c.trusted, &changed).unwrap();
    task.input_hash = fp.input;
    task.input.hash = task.input_hash.clone();
    task.input.byte_len = crate::graph::packet::canonical_json(&changed)
        .unwrap()
        .len() as u64;
    task.key = crate::jobs::tasks::task_key(&task).unwrap();
    let other = genwire::prepare(&c.trusted, &task, &changed, DispatchPurpose::Task).unwrap();
    assert_ne!(p.bound.wire_hash, other.bound.wire_hash);
    assert_ne!(p.bound.bounds_fingerprint, other.bound.bounds_fingerprint);
    let c = case(
        ServiceRole::Generate,
        generation_input(),
        "gpt-4.1-2025-04-14",
        "https://api.openai.com/v1/chat/completions",
        "output_limit_field=\"max_tokens\"\n",
        |_| {},
    );
    let p = genwire::prepare(
        &c.trusted,
        &c.spec.tasks[0],
        &generation_input(),
        DispatchPurpose::Task,
    )
    .unwrap();
    assert_eq!(
        p.bound.billable_bounds[&BillableClass::Output],
        TokenBound::Unknown
    );
}

#[test]
fn usage_partitions_missing_details_and_model_violation_preserve_paid_counts() {
    let c = case(
        ServiceRole::Generate,
        generation_input(),
        "test-model",
        "https://gateway.example/chat",
        "",
        |_| {},
    );
    let p = genwire::prepare(
        &c.trusted,
        &c.spec.tasks[0],
        &generation_input(),
        DispatchPurpose::Task,
    )
    .unwrap();
    let usage = genwire::observe(&p, &reply(&chat()));
    let KnownOrUnknown::Known(u) = usage.usage else {
        panic!()
    };
    assert_eq!(
        u.billable_units[&BillableClass::Input],
        KnownOrUnknown::Known(7)
    );
    assert_eq!(
        u.billable_units[&BillableClass::CachedInput],
        KnownOrUnknown::Known(3)
    );
    assert_eq!(
        u.billable_units[&BillableClass::Output],
        KnownOrUnknown::Known(6)
    );
    assert_eq!(
        u.billable_units[&BillableClass::Reasoning],
        KnownOrUnknown::Known(2)
    );
    let header_reply = TransportReply::new(
        200,
        vec![("X-Request-ID".into(), "header-id".into())],
        serde_json::to_vec(&chat()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        genwire::observe(&p, &header_reply)
            .provider_request_id
            .as_deref(),
        Some("header-id")
    );
    let mut v = chat();
    v["usage"]
        .as_object_mut()
        .unwrap()
        .remove("completion_tokens_details");
    let KnownOrUnknown::Known(u) = genwire::observe(&p, &reply(&v)).usage else {
        panic!()
    };
    assert_eq!(
        u.billable_units[&BillableClass::Output],
        KnownOrUnknown::Unknown
    );
    let c = case(
        ServiceRole::Generate,
        generation_input(),
        "gpt-4.1-2025-04-14",
        "https://api.openai.com/v1/chat/completions",
        "",
        |_| {},
    );
    let p = genwire::prepare(
        &c.trusted,
        &c.spec.tasks[0],
        &generation_input(),
        DispatchPurpose::Task,
    )
    .unwrap();
    let usage = genwire::observe(&p, &reply(&chat()));
    assert!(usage.contract_violation.is_some());
    assert_eq!(usage.computed_cost, KnownOrUnknown::Unknown);
    assert!(matches!(usage.usage, KnownOrUnknown::Known(_)));
    let mut malformed = chat();
    malformed["model"] = "gpt-4.1-2025-04-14".into();
    malformed["usage"]["completion_tokens_details"]["reasoning_tokens"] = 99.into();
    let observed = genwire::observe(&p, &reply(&malformed));
    assert!(observed.contract_violation.is_none());
    assert_eq!(observed.computed_cost, KnownOrUnknown::Unknown);
    assert!(genwire::decode(&p, &reply(&malformed)).is_err());
}

#[test]
fn embedding_output_details_without_total_never_settle_input_only_cost() {
    let c = case(
        ServiceRole::Embed,
        embedding_input(),
        "test-model",
        "https://gateway.example/embeddings",
        "",
        |_| {},
    );
    let mut p = embed::prepare(
        &c.trusted,
        &c.spec.tasks[0],
        &embedding_input(),
        DispatchPurpose::Task,
    )
    .unwrap();
    p.bound.rate_card = Some(RateCard {
        id: "fixture".into(),
        version: 1,
        fingerprint: hash("rates"),
        currency: Currency::new("USD").unwrap(),
        validity: PriceValidity::DispatchLocked {
            valid_from_utc_ms: 0,
            valid_until_utc_ms: i64::MAX,
        },
        request_fee_nanounits: 0,
        rates: std::collections::BTreeMap::from([(
            BillableClass::Input,
            Rate {
                price_nanounits: 1,
                per_units: std::num::NonZeroU64::new(1).unwrap(),
            },
        )]),
    });
    for field in ["reasoning_tokens", "text_tokens"] {
        let response = json!({"model":"test-model","data":[{"index":0,"embedding":[1,2]},{"index":1,"embedding":[3,4]}],
            "usage":{"prompt_tokens":6,"total_tokens":6,"completion_tokens_details":{field:1}}});
        let observed = embed::observe(&p, &reply(&response));
        assert_eq!(observed.computed_cost, KnownOrUnknown::Unknown);
        assert!(observed.contract_violation.is_some());
        assert!(embed::decode(&p, &reply(&response)).is_err());
    }
}

fn mini_input(limit: u64) -> RemoteInput {
    let mut input = generation_input();
    if let RemoteOperation::Generate {
        max_output_tokens, ..
    } = &mut input.operation
    {
        *max_output_tokens = limit;
    }
    input
}
fn mini_case(input: RemoteInput, model: &str, url: &str, extra: &str) -> Case {
    case_with_adapter(
        ServiceRole::Generate,
        input,
        model,
        url,
        extra,
        |_| {},
        "responses-v1",
    )
}
fn mini_response() -> Value {
    json!({"object":"response","status":"completed","model":"gpt-4.1-mini-2025-04-14","incomplete_details":null,"error":null,"output":[{"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"{\"ok\":true}"}]}],"usage":{"input_tokens":10,"input_tokens_details":{"cached_tokens":3},"output_tokens":8,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":18}})
}
fn mini_card() -> RateCard {
    let mut card = RateCard {
        id: "mini-fixture".into(),
        version: 1,
        fingerprint: hash([]),
        currency: Currency::new("USD").unwrap(),
        validity: PriceValidity::DispatchLocked {
            valid_from_utc_ms: 0,
            valid_until_utc_ms: i64::MAX,
        },
        request_fee_nanounits: 0,
        rates: std::collections::BTreeMap::from(
            [
                (BillableClass::Input, 400_000_000),
                (BillableClass::CachedInput, 100_000_000),
                (BillableClass::Output, 1_600_000_000),
                (BillableClass::Reasoning, 0),
            ]
            .map(|(class, price_nanounits)| {
                (
                    class,
                    Rate {
                        price_nanounits,
                        per_units: std::num::NonZeroU64::new(1_000_000).unwrap(),
                    },
                )
            }),
        ),
    };
    card.fingerprint = budgets::rate_card_fingerprint(&card).unwrap();
    card
}

fn mini_priced_extra() -> String {
    let card = mini_card();
    let mut extra = format!(
        "[services.service.rate_card]\nid=\"mini-fixture\"\nversion=1\nfingerprint={}\ncurrency=\"USD\"\nrequest_fee_nanounits=0\n[services.service.rate_card.validity]\npricing=\"dispatch_locked\"\nvalid_from_utc_ms=0\nvalid_until_utc_ms={}\n",
        quote(card.fingerprint.as_str()),
        i64::MAX
    );
    for (class, rate) in &card.rates {
        let name = match class {
            BillableClass::Input => "input",
            BillableClass::CachedInput => "cached_input",
            BillableClass::Output => "output",
            BillableClass::Reasoning => "reasoning",
        };
        extra.push_str(&format!(
            "[services.service.rate_card.rates.{name}]\nprice_nanounits={}\nper_units={}\n",
            rate.price_nanounits, rate.per_units
        ));
    }
    extra
}

#[test]
fn responses_mini_snapshot_exact_identity_bounds_and_fingerprints() {
    let input = mini_input(4096);
    let c = mini_case(
        input.clone(),
        "gpt-4.1-mini-2025-04-14",
        "https://api.openai.com/v1/responses",
        "response_mode=\"json-schema\"\n",
    );
    let p = genwire::prepare(&c.trusted, &c.spec.tasks[0], &input, DispatchPurpose::Task).unwrap();
    let WireContract::Generation(contract) = &p.contract else {
        panic!("generation expected")
    };
    assert!(matches!(
        contract.basis,
        BoundBasis::OpenAiResponsesGpt41MiniSnapshotV1
    ));
    for (class, expected) in [
        (BillableClass::Input, 1_047_576),
        (BillableClass::CachedInput, 1_047_576),
        (BillableClass::Output, 4096),
        (BillableClass::Reasoning, 0),
    ] {
        let TokenBound::ProvenUpper {
            count,
            method,
            fingerprint,
        } = &p.bound.billable_bounds[&class]
        else {
            panic!("proven bound expected for {class:?}")
        };
        assert_eq!(*count, expected);
        assert_eq!(method, "openai.responses.gpt41mini.contract.v1");
        assert_ne!(fingerprint, &hash([]));
    }
    assert_eq!(p.bound.request_bytes, p.body.len() as u64);
    assert_eq!(p.bound.response_bytes, GENERATION_SPOOL_MAX_BYTES);
    assert_eq!(p.bound.timeout_ms, 120_000);
    assert_eq!(
        p.bound.bounds_fingerprint,
        budgets::bound_fingerprint(&p.bound).unwrap()
    );
    let c2 = mini_case(
        mini_input(2048),
        "gpt-4.1-mini-2025-04-14",
        "https://api.openai.com/v1/responses",
        "response_mode=\"json-schema\"\n",
    );
    let p2 = genwire::prepare(
        &c2.trusted,
        &c2.spec.tasks[0],
        &mini_input(2048),
        DispatchPurpose::Task,
    )
    .unwrap();
    assert_ne!(p.bound.wire_hash, p2.bound.wire_hash);
    assert_ne!(p.bound.bounds_fingerprint, p2.bound.bounds_fingerprint);
    let mut forged = c.spec.tasks[0].clone();
    forged.settings_hash = hash("forged-mini-settings");
    assert!(genwire::prepare(&c.trusted, &forged, &input, DispatchPurpose::Task).is_err());
    for limit in [32768, 32769] {
        let input = mini_input(limit);
        let c = mini_case(
            input.clone(),
            "gpt-4.1-mini-2025-04-14",
            "https://api.openai.com/v1/responses",
            "max_output_tokens=32769\n",
        );
        let result = genwire::prepare(&c.trusted, &c.spec.tasks[0], &input, DispatchPurpose::Task);
        assert_eq!(result.is_ok(), limit == 32768);
    }
}

#[test]
fn responses_mini_snapshot_alias_endpoint_and_custom_ca_remain_unknown() {
    let ca = test_paths::fixture(env!("CARGO_MANIFEST_DIR"), "tests/fixtures/p16b/ca.pem");
    let ca_extra = format!("ca_file={}\n", quote(ca.to_str().unwrap()));
    for (model, url, extra) in [
        ("gpt-4.1-mini", "https://api.openai.com/v1/responses", ""),
        (
            "gpt-4.1-mini-2025-04-14",
            "https://gateway.example/v1/responses",
            "",
        ),
        (
            "gpt-4.1-mini-2025-04-14",
            "https://api.openai.com/v1/chat/completions",
            "",
        ),
        (
            "gpt-4.1-mini-2025-04-14",
            "https://api.openai.com/v1/responses",
            ca_extra.as_str(),
        ),
    ] {
        let input = mini_input(16);
        let c = mini_case(input.clone(), model, url, extra);
        let p =
            genwire::prepare(&c.trusted, &c.spec.tasks[0], &input, DispatchPurpose::Task).unwrap();
        assert!(
            p.bound
                .billable_bounds
                .values()
                .all(|bound| matches!(bound, TokenBound::Unknown))
        );
        let limits = LifetimeLimits {
            max_cost: Some(Money::new(Currency::new("USD").unwrap(), 10_000_000_000)),
            ..LifetimeLimits::default()
        };
        assert!(budgets::quote_bound(&p.bound, &limits, 1, 1_000_000).is_err());
    }
}

#[test]
fn responses_mini_snapshot_settlement_partition_missing_details_and_violations() {
    let input = mini_input(4096);
    let c = mini_case(
        input.clone(),
        "gpt-4.1-mini-2025-04-14",
        "https://api.openai.com/v1/responses",
        "",
    );
    let mut p =
        genwire::prepare(&c.trusted, &c.spec.tasks[0], &input, DispatchPurpose::Task).unwrap();
    p.bound.rate_card = Some(mini_card());
    let v = mini_response();
    assert!(genwire::decode(&p, &reply(&v)).is_ok());
    let observed = genwire::observe(&p, &reply(&v));
    assert!(observed.contract_violation.is_none());
    let KnownOrUnknown::Known(usage) = observed.usage else {
        panic!("usage expected")
    };
    for (class, count) in [
        (BillableClass::Input, 7),
        (BillableClass::CachedInput, 3),
        (BillableClass::Output, 8),
        (BillableClass::Reasoning, 0),
    ] {
        assert_eq!(usage.billable_units[&class], KnownOrUnknown::Known(count));
    }
    assert_eq!(
        observed.computed_cost,
        KnownOrUnknown::Known(Money::new(Currency::new("USD").unwrap(), 15_900))
    );
    let mut zero_cached = v.clone();
    zero_cached["usage"]["input_tokens_details"]["cached_tokens"] = 0.into();
    assert_eq!(
        genwire::observe(&p, &reply(&zero_cached)).computed_cost,
        KnownOrUnknown::Known(Money::new(Currency::new("USD").unwrap(), 16_800))
    );
    for (details, field) in [
        ("input_tokens_details", "cached_tokens"),
        ("output_tokens_details", "reasoning_tokens"),
    ] {
        for malformed in [None, Some(json!("invalid"))] {
            let mut missing = v.clone();
            if let Some(value) = malformed {
                missing["usage"][details][field] = value;
            } else {
                missing["usage"].as_object_mut().unwrap().remove(details);
            }
            assert_eq!(
                genwire::observe(&p, &reply(&missing)).computed_cost,
                KnownOrUnknown::Unknown
            );
        }
    }
    for (field, value) in [
        ("input_tokens", json!(1_047_577)),
        ("output_tokens", json!(4097)),
    ] {
        let mut excess = v.clone();
        excess["usage"][field] = value;
        excess["usage"]["total_tokens"] = (excess["usage"]["input_tokens"].as_u64().unwrap()
            + excess["usage"]["output_tokens"].as_u64().unwrap())
        .into();
        let observed = genwire::observe(&p, &reply(&excess));
        assert!(observed.contract_violation.is_some(), "{field}");
        assert_eq!(observed.computed_cost, KnownOrUnknown::Unknown);
        assert!(genwire::decode(&p, &reply(&excess)).is_err());
    }
    let mut reasoning = v.clone();
    reasoning["usage"]["output_tokens_details"]["reasoning_tokens"] = 1.into();
    assert!(
        genwire::observe(&p, &reply(&reasoning))
            .contract_violation
            .is_some()
    );
    assert_eq!(
        genwire::observe(&p, &reply(&reasoning)).computed_cost,
        KnownOrUnknown::Unknown
    );
    assert!(genwire::decode(&p, &reply(&reasoning)).is_err());
    reasoning["usage"]["input_tokens"] = "malformed".into();
    assert!(
        genwire::observe(&p, &reply(&reasoning))
            .contract_violation
            .is_some()
    );
    assert_eq!(
        genwire::observe(&p, &reply(&reasoning)).computed_cost,
        KnownOrUnknown::Unknown
    );
    assert!(genwire::decode(&p, &reply(&reasoning)).is_err());
    let mut wrong_model = v;
    wrong_model["model"] = "gpt-4.1-mini".into();
    assert!(
        genwire::observe(&p, &reply(&wrong_model))
            .contract_violation
            .is_some()
    );
    assert!(genwire::decode(&p, &reply(&wrong_model)).is_err());
}

#[test]
fn responses_mini_snapshot_finite_quote_preserves_global_outstanding_holds() {
    let input = mini_input(4096);
    let c = mini_case(
        input.clone(),
        "gpt-4.1-mini-2025-04-14",
        "https://api.openai.com/v1/responses",
        &mini_priced_extra(),
    );
    let p = genwire::prepare(&c.trusted, &c.spec.tasks[0], &input, DispatchPurpose::Task).unwrap();
    assert_eq!(
        p.bound.quoted_allowance,
        Some(Money::new(Currency::new("USD").unwrap(), 530_341_600))
    );
    let limits = LifetimeLimits {
        requests: 994,
        concurrency: 1,
        attempts_per_task: 1,
        max_cost: Some(Money::new(Currency::new("USD").unwrap(), 10_000_000_000)),
        ..LifetimeLimits::default()
    };
    let allowance = budgets::quote_bound(&p.bound, &limits, 1, 1_000_000).unwrap();
    assert_eq!(allowance.cost, p.bound.quoted_allowance);
    let mut holds = budgets::zero();
    for _ in 0..18 {
        budgets::fits(&budgets::zero(), &holds, &allowance, &limits).unwrap();
        budgets::add(&mut holds, &allowance).unwrap();
    }
    // Nineteenth reservation is blocked while unknown settlements retain all
    // eighteen outstanding reservations. No budget reset occurs.
    assert!(budgets::fits(&budgets::zero(), &holds, &allowance, &limits).is_err());
    assert_eq!(holds.cost.unwrap().nanounits(), 9_546_148_800);
}

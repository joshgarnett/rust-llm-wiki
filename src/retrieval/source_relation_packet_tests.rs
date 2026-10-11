use super::*;
use crate::retrieval::source_relations::{self, NominationLimits, SourceBinding, ValidationLimits};
use std::time::Duration;

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}
fn id(value: &str) -> RecordId {
    RecordId::new(value).unwrap()
}
fn source(text: &str) -> SourceBinding {
    SourceBinding {
        source_id: id("fictional_source"),
        source_revision: id("fictional_revision"),
        raw_hash: Blake3Hash::digest(text),
    }
}
fn preparation() -> PreparationBinding {
    PreparationBinding {
        recipe_id: id(RECIPE),
        recipe_hash: Blake3Hash::digest(RECIPE),
        prompt_hash: Blake3Hash::digest(INSTRUCTIONS),
        schema_hash: Blake3Hash::digest(serde_json::to_vec(&output_schema()).unwrap()),
        model_identity_hash: Blake3Hash::digest("mock-model"),
        settings_hash: Blake3Hash::digest("finite-settings"),
        retained_generation_id: id("retained_mock_generation"),
        retained_generation_hash: Blake3Hash::digest("retained-artifact"),
    }
}
fn allowance() -> AssemblyLimits {
    AssemblyLimits {
        max_hash_bytes: 256 * 1024,
        deadline: deadline(),
    }
}
fn data(packet: &SourceRelationPacket<'_>) -> Value {
    let RemoteOperation::Generate { data, .. } = &packet.remote_input().operation else {
        panic!("expected generation")
    };
    serde_json::from_str(data).unwrap()
}
fn response(first: &str, last: &str) -> Value {
    json!({"schema":RAW_SCHEMA,"nodes":[{"id":"r","kind":"rule","scope":"ordinary procedure","span_id":first},{"id":"c","kind":"condition","scope":"conditional supporting procedure","span_id":last}],"edges":[{"from":"r","to":"c","kind":"requires","evidence_span_id":last}]})
}

#[test]
fn unicode_fragment_reassembly_preserves_every_byte_and_descriptor_is_complete() {
    let text = format!(
        "# Header\r\n{}\n\n{}\n",
        "é😀e\u{301} Ω\t".repeat(300),
        "background".repeat(300)
    );
    let binding = source(&text);
    let packet = build(
        AdmittedOriginal {
            binding: &binding,
            text: &text,
        },
        2048,
        deadline(),
    )
    .unwrap();
    let input = data(&packet);
    assert_eq!(input["schema"], INPUT_SCHEMA);
    let fragments = input["fragments"].as_array().unwrap();
    assert!(fragments.len() > 3);
    assert_eq!(fragments.len(), packet.fragment_count());
    let mut joined = String::new();
    for (index, fragment) in fragments.iter().enumerate() {
        assert_eq!(fragment["id"], format!("span_{index:06}"));
        let text = fragment["text"].as_str().unwrap();
        assert!(!text.is_empty());
        assert!(text.len() <= FRAGMENT_BYTES);
        joined.push_str(text);
    }
    assert_eq!(joined.as_bytes(), text.as_bytes());
    assert_eq!(
        packet.serialized_input_bytes(),
        serde_json::to_vec(packet.remote_input()).unwrap().len()
    );
    assert!(packet.serialized_input_bytes() <= MAX_REMOTE_INPUT_BYTES);
    assert!(input.get("query").is_none());
    assert!(input.get("labels").is_none());
    let rebuilt = build(
        AdmittedOriginal {
            binding: &binding,
            text: &text,
        },
        2048,
        deadline(),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_vec(packet.remote_input()).unwrap(),
        serde_json::to_vec(rebuilt.remote_input()).unwrap()
    );
}

#[test]
fn distant_relation_resolves_local_spans_and_metadata_then_validates_once() {
    let text = format!(
        "Ordinary procedure requires notice.\n{}\nIf appointed abroad, obtain separate approval.\n",
        "unrelated background\n".repeat(180)
    );
    let binding = source(&text);
    let packet = build(
        AdmittedOriginal {
            binding: &binding,
            text: &text,
        },
        2048,
        deadline(),
    )
    .unwrap();
    let input = data(&packet);
    let fragments = input["fragments"].as_array().unwrap();
    let first = fragments[0]["id"].as_str().unwrap();
    let last = fragments.last().unwrap()["id"].as_str().unwrap();
    let prepared = preparation();
    let assembled = packet
        .assemble(
            &serde_json::to_vec(&response(first, last)).unwrap(),
            &prepared,
            allowance(),
        )
        .unwrap();
    assert_eq!(assembled.envelope.source, binding);
    assert_eq!(assembled.envelope.preparation, prepared);
    assert_eq!(assembled.span_hashes, 2);
    assert_eq!(
        assembled.span_hash_bytes,
        fragments[0]["text"].as_str().unwrap().len() as u64
            + fragments.last().unwrap()["text"].as_str().unwrap().len() as u64
    );
    assert!(assembled.envelope.nodes[1].evidence.span.start() > 3000);
    let bytes = serde_json::to_vec(&assembled.envelope).unwrap();
    let original = AdmittedOriginal {
        binding: &binding,
        text: &text,
    };
    let valid = source_relations::parse(
        &bytes,
        original,
        &prepared,
        ValidationLimits {
            max_hash_bytes: 64 * 1024,
            deadline: deadline(),
        },
    )
    .unwrap();
    let expected = text.len() as u64
        + assembled
            .envelope
            .nodes
            .iter()
            .map(|n| n.evidence.span.len())
            .sum::<u64>()
        + assembled
            .envelope
            .edges
            .iter()
            .map(|e| e.evidence.span.len())
            .sum::<u64>();
    assert_eq!(valid.validation_hash_bytes(), expected);
    let out = valid
        .nominate(
            &[id("r")],
            NominationLimits {
                max_candidates: 80,
                max_span_bytes: 1024,
                max_text_bytes: 12000,
                max_node_visits: 80,
                max_edge_visits: 80,
                max_depth: 4,
                deadline: deadline(),
            },
        )
        .unwrap();
    assert_eq!(out.dependencies.len(), 1);
    assert!(
        out.spans
            .iter()
            .any(|s| s.text.contains("If appointed abroad"))
    );
    for span in &out.spans {
        assert_eq!(span.text, span.evidence.span.slice(&text).unwrap());
    }
}

#[test]
fn raw_ids_fields_duplicates_and_truncation_fail_without_salvage() {
    let text = "Rule applies. If abroad, condition applies.";
    let binding = source(text);
    let packet = build(
        AdmittedOriginal {
            binding: &binding,
            text,
        },
        1024,
        deadline(),
    )
    .unwrap();
    let base = response("span_000000", "span_000000");
    let prepared = preparation();
    let reject = |value: &Value| {
        assert!(
            packet
                .assemble(&serde_json::to_vec(value).unwrap(), &prepared, allowance())
                .is_err()
        )
    };
    for path in [
        "node_span",
        "edge_span",
        "from",
        "to",
        "root",
        "node",
        "edge",
        "node_duplicate",
        "edge_duplicate",
        "schema",
        "scope",
        "kind",
    ] {
        let mut value = base.clone();
        match path {
            "node_span" => value["nodes"][0]["span_id"] = json!("span_unknown"),
            "edge_span" => value["edges"][0]["evidence_span_id"] = json!("span_unknown"),
            "from" => value["edges"][0]["from"] = json!("unknown"),
            "to" => value["edges"][0]["to"] = json!("unknown"),
            "root" => value["source_hash"] = json!("model-computed-hash"),
            "node" => value["nodes"][0]["span"] = json!({"start":0,"end":1}),
            "edge" => value["edges"][0]["applicable"] = json!(true),
            "node_duplicate" => value["nodes"][1]["id"] = json!("r"),
            "edge_duplicate" => {
                let edge = value["edges"][0].clone();
                value["edges"].as_array_mut().unwrap().push(edge);
            }
            "schema" => value["schema"] = json!("future_version"),
            "scope" => value["nodes"][0]["scope"] = json!("x".repeat(257)),
            "kind" => value["nodes"][0]["kind"] = json!("accepted_fact"),
            _ => unreachable!(),
        }
        reject(&value);
    }
    for field in ["schema", "nodes", "edges"] {
        let mut value = base.clone();
        value.as_object_mut().unwrap().remove(field);
        reject(&value);
    }
    let raw = serde_json::to_string(&base).unwrap();
    for bytes in [
        raw.as_bytes()[..raw.len() - 1].to_vec(),
        format!("{raw} {{}}").into_bytes(),
        format!("```json\n{raw}\n```").into_bytes(),
        raw.replacen("\"schema\":", "\"schema\":\"x\",\"schema\":", 1)
            .into_bytes(),
        raw.replacen("\"scope\":", "\"scope\":\"x\",\"scope\":", 1)
            .into_bytes(),
        vec![0xff],
        Vec::new(),
        vec![b' '; MAX_RAW_RESPONSE_BYTES + 1],
    ] {
        assert!(packet.assemble(&bytes, &prepared, allowance()).is_err());
    }
}

#[test]
fn descriptor_ceiling_counts_actual_nested_escaping_without_truncation() {
    let text = "x".repeat(MAX_REMOTE_INPUT_BYTES + 1);
    let binding = source(&text);
    let error = build(
        AdmittedOriginal {
            binding: &binding,
            text: &text,
        },
        1024,
        deadline(),
    )
    .err()
    .unwrap();
    assert_eq!(error.code, ErrorCode::BudgetExceeded);
    assert!(error.message.contains("large-document"));
    // Source is below 256KiB, but nested JSON escaping makes descriptor exceed it.
    let text = "\"\\".repeat(55000);
    let binding = source(&text);
    assert!(text.len() < MAX_REMOTE_INPUT_BYTES);
    let error = build(
        AdmittedOriginal {
            binding: &binding,
            text: &text,
        },
        1024,
        deadline(),
    )
    .err()
    .unwrap();
    assert_eq!(error.code, ErrorCode::BudgetExceeded);
    assert!(error.message.contains("no truncation"));
    let text = "x".repeat(110017); // Fictional size stress; no retained corpus access.
    let binding = source(&text);
    let packet = build(
        AdmittedOriginal {
            binding: &binding,
            text: &text,
        },
        1024,
        deadline(),
    )
    .unwrap();
    let restored: String = data(&packet)["fragments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["text"].as_str().unwrap())
        .collect();
    assert_eq!(restored, text);
}

#[test]
fn hash_reservation_counts_distinct_referenced_fragments_not_repeated_ids() {
    let text = "One source fragment; several proposed relations.";
    let binding = source(text);
    let packet = build(
        AdmittedOriginal {
            binding: &binding,
            text,
        },
        1024,
        deadline(),
    )
    .unwrap();
    let bytes = serde_json::to_vec(&response("span_000000", "span_000000")).unwrap();
    let prepared = preparation();
    let result = packet
        .assemble(
            &bytes,
            &prepared,
            AssemblyLimits {
                max_hash_bytes: text.len() as u64,
                deadline: deadline(),
            },
        )
        .unwrap();
    assert_eq!(result.span_hash_bytes, text.len() as u64);
    assert_eq!(result.span_hashes, 1);
    let error = packet
        .assemble(
            &bytes,
            &prepared,
            AssemblyLimits {
                max_hash_bytes: text.len() as u64 - 1,
                deadline: deadline(),
            },
        )
        .err()
        .unwrap();
    assert_eq!(error.code, ErrorCode::BudgetExceeded);
    assert!(error.details.is_null()); // Preflight refused before hashing.
    assert!(
        packet
            .assemble(
                &bytes,
                &prepared,
                AssemblyLimits {
                    deadline: Instant::now(),
                    ..allowance()
                }
            )
            .is_err()
    );
}

#[test]
fn empty_complete_source_and_invalid_token_bound_are_explicit() {
    let binding = source("");
    let packet = build(
        AdmittedOriginal {
            binding: &binding,
            text: "",
        },
        1024,
        deadline(),
    )
    .unwrap();
    assert_eq!(packet.fragment_count(), 0);
    assert_eq!(data(&packet)["fragments"], json!([]));
    let result = packet
        .assemble(
            &serde_json::to_vec(&json!({"schema":RAW_SCHEMA,"nodes":[],"edges":[]})).unwrap(),
            &preparation(),
            allowance(),
        )
        .unwrap();
    assert_eq!(result.span_hashes, 0);
    assert_eq!(result.span_hash_bytes, 0);
    assert!(
        build(
            AdmittedOriginal {
                binding: &binding,
                text: ""
            },
            0,
            deadline()
        )
        .is_err()
    );
}

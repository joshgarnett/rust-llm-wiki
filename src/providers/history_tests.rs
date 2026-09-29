//! Actual sealed wire preparation followed by local original-contract decoding.
use super::{embedding_wire, generation_wire, history, types::*, wire_tests::common::*};
use crate::{
    changes::ReadDependency,
    domain::*,
    graph::packet::canonical_json,
    jobs::{AttemptBound, TaskSpec, budgets, tasks},
    vault::ExpectedState,
};
use serde_json::{Value, json};
use std::sync::atomic::Ordering;
fn generation_input() -> RemoteInput {
    RemoteInput {
        version: 1,
        operation: RemoteOperation::Generate {
            instructions: "Return only the requested object".into(),
            data: "private source input".into(),
            output_schema: json!({"type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"],"additionalProperties":false}),
            max_output_tokens: 16,
        },
    }
}
fn generation_case() -> Case {
    case(
        ServiceRole::Generate,
        generation_input(),
        "gpt-4.1-2025-04-14",
        "https://api.openai.com/v1/chat/completions",
        "",
        |_| {},
    )
}
fn run_task(c: &Case, input: &RemoteInput) -> TaskSpec {
    let mut task = c.spec.tasks[0].clone();
    task.input.path =
        VaultRelativePath::new(format!("runs/{}/inputs/original.json", c.spec.run_id)).unwrap();
    let path = c.fs.root().resolve(&task.input.path).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let bytes = canonical_json(input).unwrap();
    private(&path, &bytes);
    task.input.hash = hash(&bytes);
    task.input.byte_len = bytes.len() as u64;
    task.input_hash = hash(&bytes);
    task.key = tasks::task_key(&task).unwrap();
    task
}
fn prepared_generation(c: &Case, task: &TaskSpec) -> PreparedWire {
    let mut p =
        generation_wire::prepare(&c.trusted, task, &generation_input(), DispatchPurpose::Task)
            .unwrap();
    p.bound.profile_fingerprint = Some(c.trusted.summary().profile_fingerprint);
    p.bound.bounds_fingerprint = budgets::bound_fingerprint(&p.bound).unwrap();
    p
}
fn reply(value: Value) -> TransportReply {
    TransportReply::new(200, vec![], serde_json::to_vec(&value).unwrap()).unwrap()
}
fn chat(model: &str, content: &str) -> TransportReply {
    reply(
        json!({"model":model,"choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":content}}],"usage":{"prompt_tokens":10,"completion_tokens":8,"total_tokens":18}}),
    )
}
#[test]
fn historical_generation_decodes_original_model_after_config_source_and_cache_change() {
    let c = generation_case();
    let mut task = run_task(&c, &generation_input());
    private(&c.fs.root().path().join("source.txt"), b"original source");
    task.source_bindings.push(ReadDependency {
        path: VaultRelativePath::new("source.txt").unwrap(),
        expected: ExpectedState::Hash(hash("original source")),
    });
    task.key = tasks::task_key(&task).unwrap();
    let mut p = prepared_generation(&c, &task);
    let original_wire = p.bound.wire_hash.clone();
    p.headers.insert(
        "Authorization".into(),
        "fixture-secret-that-must-not-be-retained".into(),
    );
    history::retain(&c.fs, &task, &mut p).unwrap();
    assert_eq!(p.bound.wire_hash, original_wire);
    let codec = p.bound.codec.clone().unwrap();
    let snapshot = std::fs::read(c.fs.root().resolve(&codec.path).unwrap()).unwrap();
    let text = String::from_utf8(snapshot).unwrap();
    assert!(!text.contains("fixture-secret-that-must-not-be-retained"));
    assert!(!text.contains("private source input"));
    assert!(!text.contains("Authorization"));
    private(
        &c.config,
        b"current config has changed, including model and credentials",
    );
    private(&c.fs.root().path().join("source.txt"), b"edited source");
    if c.fs.root().path().join(".wiki/cache").exists() {
        std::fs::remove_dir_all(c.fs.root().path().join(".wiki/cache")).unwrap();
    }
    assert!(c.trusted.recheck(&c.fs).is_err());
    let restored = history::restore(&c.fs, &task, &p.bound, DispatchPurpose::Task).unwrap();
    assert!(restored.body.is_empty());
    assert!(restored.headers.is_empty());
    assert_eq!(restored.url, "lwiki:historical-decode-only");
    assert!(matches!(
        generation_wire::decode(&restored, &chat("gpt-4.1-2025-04-14", "{\"ok\":true}")).unwrap(),
        ValidatedOutput::Generation { .. }
    ));
    assert!(generation_wire::decode(&restored, &chat("new-model", "{\"ok\":true}")).is_err());
    assert!(
        generation_wire::decode(
            &restored,
            &chat("gpt-4.1-2025-04-14", "{\"invented\":true}")
        )
        .is_err()
    );
    assert!(
        generation_wire::observe(&restored, &chat("new-model", "{\"ok\":true}"))
            .contract_violation
            .is_some()
    );
    assert_eq!(c.inputs.0.load(Ordering::SeqCst), 0);
    assert_eq!(c.runner.calls.load(Ordering::SeqCst), 0);
}
#[test]
fn historical_codec_retain_is_idempotent_and_does_not_change_wire_hash() {
    let c = generation_case();
    let task = run_task(&c, &generation_input());
    let mut p = prepared_generation(&c, &task);
    history::retain(&c.fs, &task, &mut p).unwrap();
    let first = p.bound.clone();
    history::retain(&c.fs, &task, &mut p).unwrap();
    assert_eq!(p.bound, first);
    let path =
        c.fs.root()
            .resolve(&p.bound.codec.as_ref().unwrap().path)
            .unwrap();
    private(&path, b"corrupt");
    assert!(history::restore(&c.fs, &task, &p.bound, DispatchPurpose::Task).is_err());
    assert!(history::retain(&c.fs, &task, &mut p).is_err());
    assert_eq!(std::fs::read(path).unwrap(), b"corrupt");
}
#[test]
fn historical_restore_rejects_missing_codec_descriptor_and_changed_complete_task_or_bound() {
    let c = generation_case();
    let task = run_task(&c, &generation_input());
    let mut p = prepared_generation(&c, &task);
    assert!(history::restore(&c.fs, &task, &p.bound, DispatchPurpose::Task).is_err());
    history::retain(&c.fs, &task, &mut p).unwrap();
    let mut changed = task.clone();
    changed.priority += 1;
    assert_eq!(tasks::task_key(&changed).unwrap(), task.key);
    assert!(history::restore(&c.fs, &changed, &p.bound, DispatchPurpose::Task).is_err());
    let mut bound = p.bound.clone();
    bound.requested_model = Some("different-model".into());
    bound.bounds_fingerprint = budgets::bound_fingerprint(&bound).unwrap();
    assert!(history::restore(&c.fs, &task, &bound, DispatchPurpose::Task).is_err());
    assert!(
        history::restore(
            &c.fs,
            &task,
            &p.bound,
            DispatchPurpose::Probe {
                role: ServiceRole::Generate
            }
        )
        .is_err()
    );
    std::fs::remove_file(c.fs.root().resolve(&task.input.path).unwrap()).unwrap();
    assert!(history::restore(&c.fs, &task, &p.bound, DispatchPurpose::Task).is_err());
    assert!(
        c.fs.root()
            .resolve(&p.bound.codec.as_ref().unwrap().path)
            .unwrap()
            .exists()
    );
}
fn rewritten(c: &Case, original: &AttemptBound, bytes: &[u8]) -> AttemptBound {
    let mut bound = original.clone();
    let codec = bound.codec.as_mut().unwrap();
    codec.hash = hash(bytes);
    codec.byte_len = bytes.len() as u64;
    codec.path = VaultRelativePath::new(format!(
        ".wiki/state/provider-codecs/{}.json",
        codec.hash.hex()
    ))
    .unwrap();
    private(&c.fs.root().resolve(&codec.path).unwrap(), bytes);
    bound.bounds_fingerprint = budgets::bound_fingerprint(&bound).unwrap();
    bound
}
#[test]
fn historical_codec_rejects_unknown_version_fields_noncanonical_and_foreign_paths() {
    let c = generation_case();
    let task = run_task(&c, &generation_input());
    let mut p = prepared_generation(&c, &task);
    history::retain(&c.fs, &task, &mut p).unwrap();
    let bytes = std::fs::read(
        c.fs.root()
            .resolve(&p.bound.codec.as_ref().unwrap().path)
            .unwrap(),
    )
    .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    let mut version = value.clone();
    version["version"] = 2.into();
    let mut extra = value.clone();
    extra["headers"] = json!({"Authorization":"must not exist"});
    let bads = [
        canonical_json(&version).unwrap(),
        canonical_json(&extra).unwrap(),
        serde_json::to_vec_pretty(&value).unwrap(),
    ];
    for bytes in bads {
        let bound = rewritten(&c, &p.bound, &bytes);
        assert!(history::restore(&c.fs, &task, &bound, DispatchPurpose::Task).is_err());
    }
    let mut bound = p.bound.clone();
    let codec = bound.codec.as_mut().unwrap();
    codec.path =
        VaultRelativePath::new(format!("runs/other_run/codecs/{}.json", codec.hash.hex())).unwrap();
    bound.bounds_fingerprint = budgets::bound_fingerprint(&bound).unwrap();
    assert!(history::restore(&c.fs, &task, &bound, DispatchPurpose::Task).is_err());
}
#[test]
fn historical_embedding_preserves_positions_dimensions_and_original_model_basis() {
    let input = RemoteInput {
        version: 1,
        operation: RemoteOperation::Embed {
            inputs: vec![
                EmbeddingInput {
                    input_hash: hash("same"),
                    utf8: "same".into()
                };
                2
            ],
            expected_dimensions: Some(2),
            representation_fingerprint: hash("representation"),
        },
    };
    let c = case(
        ServiceRole::Embed,
        input.clone(),
        "text-embedding-3-small",
        "https://api.openai.com/v1/embeddings",
        "",
        |_| {},
    );
    let task = run_task(&c, &input);
    let mut p = embedding_wire::prepare(&c.trusted, &task, &input, DispatchPurpose::Task).unwrap();
    history::retain(&c.fs, &task, &mut p).unwrap();
    private(&c.config, b"changed config");
    let restored = history::restore(&c.fs, &task, &p.bound, DispatchPurpose::Task).unwrap();
    let good = json!({"model":"text-embedding-3-small","data":[{"index":1,"embedding":[3,4]},{"index":0,"embedding":[1,2]}]});
    let ValidatedOutput::Embeddings { vectors, .. } =
        embedding_wire::decode(&restored, &reply(good.clone())).unwrap()
    else {
        panic!("embedding output required")
    };
    assert_eq!(vectors, vec![vec![1.0, 2.0], vec![3.0, 4.0]]);
    let mut bad = good.clone();
    bad["data"][0]["index"] = 0.into();
    assert!(embedding_wire::decode(&restored, &reply(bad)).is_err());
    let mut bad = good;
    bad["data"][0]["embedding"] = json!([1, 2, 3]);
    assert!(embedding_wire::decode(&restored, &reply(bad)).is_err());
    assert_eq!(c.inputs.0.load(Ordering::SeqCst), 0);
    assert_eq!(c.runner.calls.load(Ordering::SeqCst), 0);
}
#[test]
fn historical_retain_binds_generic_descriptor_without_run_directory_requirement() {
    let c = generation_case();
    let task = c.spec.tasks[0].clone();
    let mut p = prepared_generation(&c, &task);
    history::retain(&c.fs, &task, &mut p).unwrap();
    assert!(
        p.bound
            .codec
            .as_ref()
            .unwrap()
            .path
            .as_str()
            .starts_with(".wiki/state/provider-codecs/")
    );
    history::restore(&c.fs, &task, &p.bound, DispatchPurpose::Task).unwrap();
}

#[test]
fn responses_codec_retains_provider_grammar_and_recovers_without_current_config() {
    let mut input = generation_input();
    if let RemoteOperation::Generate { output_schema, .. } = &mut input.operation {
        *output_schema = json!({"type":"object","properties":{"ok":{"type":"boolean"},
            "details":{"type":"object","properties":{"note":{"type":"string","minLength":1}},"required":[],"additionalProperties":false}},
            "required":["ok","details"],"additionalProperties":false});
    }
    let c = case_with_adapter(
        ServiceRole::Generate,
        input.clone(),
        "gateway-model",
        "https://gateway.example/v1/responses",
        "response_mode=\"json-schema\"\n",
        |_| {},
        "responses-v1",
    );
    let task = run_task(&c, &input);
    let mut prepared =
        generation_wire::prepare(&c.trusted, &task, &input, DispatchPurpose::Task).unwrap();
    history::retain(&c.fs, &task, &mut prepared).unwrap();
    let codec = prepared.bound.codec.as_ref().unwrap();
    let retained: Value =
        serde_json::from_slice(&std::fs::read(c.fs.root().resolve(&codec.path).unwrap()).unwrap())
            .unwrap();
    assert_eq!(retained["contract"]["surface"], "responses");
    assert!(retained["contract"]["provider_schema"].is_object());
    std::fs::remove_file(c.temp.path().join("providers.toml")).unwrap();
    let restored = history::restore(&c.fs, &task, &prepared.bound, DispatchPurpose::Task).unwrap();
    let response = |text: &str| {
        reply(
            json!({"object":"response","status":"completed","model":"gateway-model",
        "output":[{"type":"reasoning","encrypted_content":"ignored"},
            {"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":text}]}]}),
        )
    };
    assert!(generation_wire::decode(&restored, &response("{\"ok\":true,\"details\":{}}")).is_ok());
    assert!(
        generation_wire::decode(
            &restored,
            &response("{\"ok\":true,\"details\":{\"note\":\"\"}}")
        )
        .is_err()
    );
    let mut bad = retained;
    bad["contract"]["provider_schema"]["properties"]["ok"]["type"] = json!("string");
    let bound = rewritten(&c, &prepared.bound, &canonical_json(&bad).unwrap());
    assert!(history::restore(&c.fs, &task, &bound, DispatchPurpose::Task).is_err());
}

#[test]
fn legacy_chat_codec_omits_new_fields_and_remains_canonical() {
    let c = generation_case();
    let task = run_task(&c, &generation_input());
    let mut p = prepared_generation(&c, &task);
    history::retain(&c.fs, &task, &mut p).unwrap();
    let bytes = std::fs::read(
        c.fs.root()
            .resolve(&p.bound.codec.as_ref().unwrap().path)
            .unwrap(),
    )
    .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(value["contract"].get("surface").is_none());
    assert!(value["contract"].get("provider_schema").is_none());
    assert!(history::restore(&c.fs, &task, &p.bound, DispatchPurpose::Task).is_ok());
}

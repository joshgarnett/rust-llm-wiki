//! Existing sealed-wire fixtures, with no retained descriptor or send authority.
use super::*;
use crate::providers::{generation_wire, wire_tests::common::*};
use serde_json::json;
use std::sync::atomic::Ordering;

fn input() -> RemoteInput {
    RemoteInput {
        version: 1,
        operation: RemoteOperation::Generate {
            instructions: "Return an object".into(),
            data: "complete fixture source".into(),
            output_schema: json!({"type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"],"additionalProperties":false}),
            max_output_tokens: 4096,
        },
    }
}
fn fixture() -> Case {
    case_with_adapter(
        ServiceRole::Generate,
        input(),
        "gpt-4.1-mini-2025-04-14",
        "https://api.openai.com/v1/responses",
        "response_mode=\"json-schema\"\n",
        |_| {},
        "responses-v1",
    )
}
fn plan(c: &Case, input: &RemoteInput) -> GenerationTaskPlan {
    plan_task(&c.trusted, input, TaskStage::PrepareSourceRelations, vec![]).unwrap()
}
fn replace_descriptor(plan: &mut GenerationTaskPlan, bytes: Vec<u8>) {
    plan.descriptor = bytes;
    plan.task.input.byte_len = plan.descriptor.len() as u64;
    plan.task.input.hash = hash(&plan.descriptor);
    plan.task.input_hash = plan.task.input.hash.clone();
    plan.task.key = crate::jobs::tasks::task_key(&plan.task).unwrap();
}

#[test]
fn prepared_bound_matches_real_wire_without_retaining_or_resolving_credentials() {
    let c = fixture();
    let plan = plan(&c, &input());
    assert!(!c.fs.root().resolve(&plan.task.input.path).unwrap().exists());
    let bound = prepared_bound(&c.trusted, &plan).unwrap();
    let real =
        generation_wire::prepare(&c.trusted, &plan.task, &input(), DispatchPurpose::Task).unwrap();
    assert_eq!(bound, real.bound);
    assert_eq!(
        bound.bounds_fingerprint,
        crate::jobs::budgets::bound_fingerprint(&bound).unwrap()
    );
    assert!(!c.fs.root().resolve(&plan.task.input.path).unwrap().exists());
    assert_eq!(c.inputs.0.load(Ordering::SeqCst), 0);
    assert_eq!(c.runner.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn prepared_bound_rejects_descriptor_task_and_input_identity_changes() {
    let c = fixture();
    let mut changed = plan(&c, &input());
    changed.descriptor.push(b' ');
    assert!(prepared_bound(&c.trusted, &changed).is_err());
    for field in 0..7 {
        let mut changed = plan(&c, &input());
        match field {
            0 => changed.task.key = hash("forged-key"),
            1 => changed.task.input.hash = hash("forged-descriptor"),
            2 => changed.task.input.byte_len += 1,
            3 => changed.task.input_hash = hash("forged-input"),
            4 => changed.task.settings_hash = hash("forged-settings"),
            5 => changed.task.capability = Some(Capability::Embed),
            _ => changed.task.stage = TaskStage::Probe,
        }
        if field > 0 {
            changed.task.key = crate::jobs::tasks::task_key(&changed.task).unwrap();
        }
        assert!(
            prepared_bound(&c.trusted, &changed).is_err(),
            "field {field}"
        );
    }
    let mut noncanonical = plan(&c, &input());
    replace_descriptor(
        &mut noncanonical,
        serde_json::to_vec_pretty(&input()).unwrap(),
    );
    assert!(prepared_bound(&c.trusted, &noncanonical).is_err());
    let mut version = input();
    version.version = 2;
    assert!(prepared_bound(&c.trusted, &plan(&c, &version)).is_err());
    let wrong = RemoteInput {
        version: 1,
        operation: RemoteOperation::Embed {
            inputs: vec![EmbeddingInput {
                input_hash: hash("embed"),
                utf8: "embed".into(),
            }],
            expected_dimensions: Some(2),
            representation_fingerprint: hash("repr"),
        },
    };
    let mut wrong_plan = plan(&c, &input());
    replace_descriptor(
        &mut wrong_plan,
        crate::graph::packet::canonical_json(&wrong).unwrap(),
    );
    assert!(prepared_bound(&c.trusted, &wrong_plan).is_err());
}

#[test]
fn prepared_bound_enforces_dispatch_descriptor_byte_node_and_depth_limits() {
    let c = fixture();
    let mut too_large = plan(&c, &input());
    replace_descriptor(&mut too_large, vec![b' '; 256 * 1024 + 1]);
    assert!(prepared_bound(&c.trusted, &too_large).is_err());
    let mut nodes = input();
    if let RemoteOperation::Generate { output_schema, .. } = &mut nodes.operation {
        *output_schema = json!({"enum": vec!["value"; 8193]});
    }
    let nodes_plan = plan(&c, &nodes);
    assert!(nodes_plan.descriptor.len() < 256 * 1024);
    assert!(super::super::wire_json::parse(&nodes_plan.descriptor, 256 * 1024, 8192, 40).is_err());
    assert!(prepared_bound(&c.trusted, &nodes_plan).is_err());
    let mut nested = json!("leaf");
    for _ in 0..41 {
        nested = json!([nested]);
    }
    let mut depth = input();
    if let RemoteOperation::Generate { output_schema, .. } = &mut depth.operation {
        *output_schema = nested;
    }
    let depth_plan = plan(&c, &depth);
    assert!(super::super::wire_json::parse(&depth_plan.descriptor, 256 * 1024, 8192, 40).is_err());
    assert!(prepared_bound(&c.trusted, &depth_plan).is_err());
}

#[test]
fn prepared_bound_refuses_unsupported_schema_and_wrapped_wire_overflow() {
    let c = fixture();
    let mut unsupported = input();
    if let RemoteOperation::Generate { output_schema, .. } = &mut unsupported.operation {
        *output_schema = json!({"type":"object","additionalProperties":true});
    }
    assert!(prepared_bound(&c.trusted, &plan(&c, &unsupported)).is_err());
    let mut large = input();
    let base = crate::graph::packet::canonical_json(&large).unwrap().len();
    if let RemoteOperation::Generate { data, .. } = &mut large.operation {
        // Fill the descriptor exactly; real Responses text.format wrapping has
        // additional bytes and must independently refuse this complete input.
        *data = "x".repeat(256 * 1024 - base + data.len());
    }
    let large_plan = plan(&c, &large);
    assert_eq!(large_plan.descriptor.len(), 256 * 1024);
    assert!(
        generation_wire::prepare(&c.trusted, &large_plan.task, &large, DispatchPurpose::Task)
            .is_err()
    );
    assert!(prepared_bound(&c.trusted, &large_plan).is_err());
}

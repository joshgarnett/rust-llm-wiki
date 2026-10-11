use super::*;
fn meter() -> Meter {
    Meter {
        limits: MaterializationLimits {
            max_captured_bytes: 1024,
            max_hash_bytes: 100,
            deadline: Instant::now() + std::time::Duration::from_secs(10),
        },
        charged: 0,
        known: 0,
    }
}
#[test]
fn failed_opaque_authentication_preserves_reservation_and_known_subset() {
    let mut meter = meter();
    let mut error = invalid("fictional captured-source rejection");
    error.details = serde_json::json!({"source_relation_inputs_reserved_hash_bytes":70,"source_relation_inputs_known_hash_bytes":10});
    assert!(meter.child::<()>(Err(error)).is_err());
    assert_eq!((meter.charged, meter.known), (70, 10));
    assert_eq!(meter.remaining().unwrap(), 30);
}
#[test]
fn downstream_failure_accumulates_without_resetting_input_reservation() {
    let mut meter = meter();
    meter.add(70, 10).unwrap();
    let mut error = invalid("fictional invalid generated relation");
    error.details = serde_json::json!({"source_relation_store_hash_bytes_consumed":12});
    assert!(meter.child::<()>(Err(error)).is_err());
    assert_eq!((meter.charged, meter.known), (82, 22));
    assert!(meter.digest(&[0; 19]).is_err());
    assert_eq!((meter.charged, meter.known), (82, 22));
}
#[test]
fn completed_child_work_remains_charged_after_deadline() {
    let mut meter = meter();
    meter.limits.deadline = Instant::now();
    meter.add(20, 20).unwrap();
    assert!(meter.remaining().is_err());
    assert_eq!((meter.charged, meter.known), (20, 20));
}

use super::super::source_relation_inputs::tests::Fixture;
use crate::providers::types::RemoteOperation;
fn prepared(shared: bool) -> (Fixture, Vec<u8>, Vec<u8>, AttemptRef) {
    let mut f = Fixture::new(shared);
    let authenticated = f.authenticate().unwrap();
    let packet = packet::build(
        relations::AdmittedOriginal {
            binding: &authenticated.binding,
            text: &authenticated.text,
        },
        512,
        Instant::now() + std::time::Duration::from_secs(10),
    )
    .unwrap();
    let descriptor = crate::graph::packet::canonical_json(packet.remote_input()).unwrap();
    let (instructions, data, schema) = match &packet.remote_input().operation {
        RemoteOperation::Generate {
            instructions,
            data,
            output_schema,
            ..
        } => (instructions, data, output_schema),
        _ => unreachable!(),
    };
    f.task.input_hash = Blake3Hash::digest(&descriptor);
    f.task.input.hash = f.task.input_hash.clone();
    f.task.input.byte_len = descriptor.len() as u64;
    f.task.prompt_hash = Some(Blake3Hash::digest(
        crate::graph::packet::canonical_json(&(instructions, data)).unwrap(),
    ));
    f.task.schema_hash = Some(Blake3Hash::digest(
        crate::graph::packet::canonical_json(schema).unwrap(),
    ));
    f.task.key = crate::jobs::tasks::task_key(&f.task).unwrap();
    let attempt = AttemptRef {
        run_id: RecordId::new("fictional_run").unwrap(),
        task_key: f.task.key.clone(),
        attempt_id: RecordId::new("fictional_attempt").unwrap(),
        number: 1,
        request_hash: Blake3Hash::digest(b"sealed fictional request"),
    };
    let response = br#"{"schema":"lwiki.source-relation-response.v1","nodes":[{"id":"rule_one","kind":"rule","scope":"Notice required","span_id":"span_000000"},{"id":"condition_one","kind":"requirement","scope":"Notice","span_id":"span_000000"}],"edges":[{"from":"rule_one","to":"condition_one","kind":"requires","evidence_span_id":"span_000000"}]}"#.to_vec();
    (f, descriptor, response, attempt)
}
fn generous() -> MaterializationLimits {
    MaterializationLimits {
        max_captured_bytes: 1024 * 1024,
        max_hash_bytes: 32 * 1024 * 1024,
        deadline: Instant::now() + std::time::Duration::from_secs(10),
    }
}
fn run(
    f: &Fixture,
    descriptor: &[u8],
    response: &[u8],
    attempt: &AttemptRef,
    limits: MaterializationLimits,
) -> Result<MaterializedRelations> {
    materialize(
        &f.fs,
        &f.vault,
        &f.task,
        &f.captured,
        descriptor,
        response,
        attempt,
        "2026-10-10T00:00:00Z",
        limits,
    )
}
#[test]
fn exact_complete_source_generates_replayable_bound_pair_in_both_capture_layouts() {
    for shared in [false, true] {
        let (f, descriptor, response, attempt) = prepared(shared);
        let out = run(&f, &descriptor, &response, &attempt, generous()).unwrap();
        let replay = run(&f, &descriptor, &response, &attempt, generous()).unwrap();
        assert_eq!(out.raw.write.proposed, replay.raw.write.proposed);
        assert_eq!(
            out.annotation.write.proposed,
            replay.annotation.write.proposed
        );
        assert_ne!(out.raw.reference.path, out.annotation.reference.path);
        for artifact in [&out.raw, &out.annotation] {
            assert_eq!(
                artifact.reference.hash,
                Blake3Hash::digest(artifact.write.proposed.as_ref().unwrap())
            );
            assert_eq!(artifact.write.target, artifact.reference.path);
            assert_eq!(artifact.write.expected, crate::vault::ExpectedState::Absent);
        }
        let loaded = store::decode_annotation(
            out.annotation.write.proposed.as_ref().unwrap(),
            &out.annotation.reference,
            &out.raw.reference,
            store::StoreLimits {
                max_hash_bytes: 32 * 1024 * 1024,
                deadline: generous().deadline,
            },
        )
        .unwrap();
        assert_eq!(loaded.artifact.task, f.task);
        assert_eq!(loaded.artifact.envelope.nodes.len(), 2);
        assert_eq!(loaded.artifact.envelope.edges.len(), 1);
        assert_eq!(
            loaded
                .artifact
                .envelope
                .preparation
                .retained_generation_hash,
            out.raw.reference.hash
        );
        let captured = f.captured.values().map(|b| b.len() as u64).sum::<u64>();
        assert_eq!(out.captured_bytes, captured);
        assert_eq!(out.reserved_hash_bytes - out.known_hash_bytes, 6 * captured);
    }
}
#[test]
fn descriptor_source_fingerprints_attempt_and_generated_spans_cannot_forge_pair() {
    for case in 0..8 {
        let (mut f, mut descriptor, mut response, mut attempt) = prepared(false);
        match case {
            0 => descriptor.push(b' '), // semantically identical, wrong exact bytes
            1 => {
                f.captured.get_mut(&f.content).unwrap().push(b'!');
            }
            2 => f.task.input.hash = Blake3Hash::digest(b"forged"),
            3 => f.task.prompt_hash = Some(Blake3Hash::digest(b"forged")),
            4 => attempt.task_key = Blake3Hash::digest(b"unrelated task"),
            5 => attempt.number = 0,
            6 => {
                response = String::from_utf8(response)
                    .unwrap()
                    .replace("span_000000", "span_unknown")
                    .into_bytes()
            }
            7 => {
                f.captured.get_mut(&f.content).unwrap().push(b'!');
                f.rebind();
                f.task.key = crate::jobs::tasks::task_key(&f.task).unwrap();
                attempt.task_key = f.task.key.clone();
            }
            _ => unreachable!(),
        }
        let error = run(&f, &descriptor, &response, &attempt, generous())
            .err()
            .unwrap();
        assert!(
            error.details["source_relation_materialization_reserved_hash_bytes"]
                .as_u64()
                .unwrap()
                > 0,
            "case {case}"
        );
        assert_eq!(
            error.details["source_relation_materialization_actual_total_hash_bytes"],
            "unavailable"
        );
    }
}
#[test]
fn whole_materialization_budget_is_cumulative_and_retains_failed_work() {
    let (f, descriptor, response, attempt) = prepared(false);
    let baseline = run(&f, &descriptor, &response, &attempt, generous()).unwrap();
    let error = run(
        &f,
        &descriptor,
        &response,
        &attempt,
        MaterializationLimits {
            max_hash_bytes: baseline.reserved_hash_bytes - 1,
            ..generous()
        },
    )
    .err()
    .unwrap();
    assert_eq!(error.code, ErrorCode::BudgetExceeded);
    let charged = error.details["source_relation_materialization_reserved_hash_bytes"]
        .as_u64()
        .unwrap();
    assert!(charged > 7 * baseline.captured_bytes);
    assert!(charged < baseline.reserved_hash_bytes);
    assert_eq!(
        error.details["source_relation_materialization_actual_total_hash_bytes"],
        "unavailable"
    );
    let expired = run(
        &f,
        &descriptor,
        &response,
        &attempt,
        MaterializationLimits {
            deadline: Instant::now(),
            ..generous()
        },
    )
    .err()
    .unwrap();
    assert_eq!(expired.code, ErrorCode::BudgetExceeded);
    assert_eq!(
        expired.details["source_relation_materialization_reserved_hash_bytes"],
        0
    );
}

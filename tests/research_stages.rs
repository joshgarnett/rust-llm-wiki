extern crate lwiki;
#[allow(dead_code)]
#[path = "fixtures/p15/common.rs"]
mod common;
use common::*;
use lwiki::{
    catalog::{Catalog, CatalogGraphValidator},
    changes::{ChangeDraft, ChangeEngine},
    jobs::*,
    research::stages,
    vault::WriterPermit,
};
use serde_json::json;
use std::{collections::BTreeMap, sync::atomic::Ordering, time::Duration};

#[test]
fn applied_local_output_without_finished_task_is_reused_after_clock_advances() {
    let (_temp, fs, job, spec, clock) = fixture(1, |spec| {
        let task = &mut spec.tasks[0];
        task.stage = TaskStage::InspectExisting;
        task.capability = None;
        task.key = lwiki::jobs::tasks::task_key(task).unwrap();
    });
    let task = &spec.tasks[0];
    let response = json!({"optional":null,"fraction":1.5,"items":[]});
    let (output, operation) = stages::output_write(
        &spec.vault_id,
        &spec.run_id,
        task,
        None,
        response.clone(),
        spec.created_at_utc_ms,
    )
    .unwrap();
    // This is the real persisted state at the boundary between canonical apply
    // and TaskFinished. Do not simulate completion in a detached state machine.
    let writer = WriterPermit::acquire(fs.root(), Duration::from_secs(5)).unwrap();
    let engine = ChangeEngine::new(fs.clone()).unwrap();
    let prepared = engine
        .prepare(
            &writer,
            ChangeDraft {
                title: "Interrupted local research publication".into(),
                origin: None,
                inverse_of: None,
                allocated_ids: BTreeMap::new(),
                read_preconditions: vec![],
                operations: vec![operation],
            },
        )
        .unwrap()
        .prepared;
    engine
        .apply(
            &writer,
            &prepared,
            &CatalogGraphValidator,
            &Catalog::new(fs.clone(), spec.vault_id.clone()),
        )
        .unwrap();
    drop(writer);
    assert_eq!(
        job.inspect().unwrap().tasks[&task.key].state,
        TaskState::Pending
    );
    clock.0.fetch_add(1000, Ordering::SeqCst);
    let restarted = JobLedger::new(
        fs.clone(),
        spec.vault_id.clone(),
        spec.run_id.clone(),
        options(clock.clone()),
    )
    .unwrap();
    restarted.replay().unwrap();
    let adopted = stages::publish_local(
        &fs,
        &restarted,
        task,
        response.clone(),
        vec![],
        spec.created_at_utc_ms + 1000,
    )
    .unwrap();
    assert_eq!(adopted, output);
    assert_eq!(
        stages::read_output(&fs, &adopted, task).unwrap().response,
        response
    );
    let inspection = restarted.inspect().unwrap();
    assert_eq!(inspection.tasks[&task.key].state, TaskState::Completed);
    assert!(inspection.attempts.is_empty());
    assert_eq!(inspection.budget.dispatched_requests, 0);
}

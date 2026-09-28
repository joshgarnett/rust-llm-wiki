//! Stable task identity, source binding and bounded dependency scheduling.
use super::types::*;
use crate::{
    changes::prepare::read_bounded,
    domain::*,
    vault::{ExpectedState, VaultFs},
};
use std::collections::{BTreeMap, BTreeSet};
pub fn task_key(task: &TaskSpec) -> Result<Blake3Hash> {
    let mut value = serde_json::to_value(task).map_err(|e| WikiError::invalid(e.to_string()))?;
    let object = value.as_object_mut().unwrap();
    for name in ["key", "priority", "dependencies"] {
        object.remove(name);
    }
    Ok(Blake3Hash::digest(
        serde_json::to_vec(&value).map_err(|e| WikiError::invalid(e.to_string()))?,
    ))
}
pub(super) fn validate(tasks: &BTreeMap<Blake3Hash, TaskInspection>) -> Result<()> {
    if tasks.len() > RUN_MAX_TASKS {
        return Err(WikiError::new(
            ErrorCode::BudgetExceeded,
            "run task ceiling exceeded",
        ));
    }
    for (key, task) in tasks {
        if key != &task.spec.key
            || key != &task_key(&task.spec)?
            || task.spec.input.byte_len > crate::changes::prepare::MAX_PAYLOAD_BYTES as u64
            || task.spec.dependencies.iter().collect::<BTreeSet<_>>().len()
                != task.spec.dependencies.len()
        {
            return Err(WikiError::invalid(
                "invalid task key, descriptor bound or duplicate dependency",
            ));
        }
        if task
            .spec
            .dependencies
            .iter()
            .any(|dep| !tasks.contains_key(dep) || dep == key)
        {
            return Err(WikiError::invalid("task dependency missing or cyclic"));
        }
    }
    let mut remaining: BTreeMap<_, usize> = tasks
        .iter()
        .map(|(key, task)| (key.clone(), task.spec.dependencies.len()))
        .collect();
    let mut reverse: BTreeMap<Blake3Hash, Vec<Blake3Hash>> = BTreeMap::new();
    for (key, task) in tasks {
        for dep in &task.spec.dependencies {
            reverse.entry(dep.clone()).or_default().push(key.clone());
        }
    }
    let mut ready: Vec<_> = remaining
        .iter()
        .filter(|(_, n)| **n == 0)
        .map(|(k, _)| k.clone())
        .collect();
    let mut visited = 0;
    while let Some(key) = ready.pop() {
        visited += 1;
        for dependent in reverse.get(&key).into_iter().flatten() {
            let n = remaining.get_mut(dependent).unwrap();
            *n -= 1;
            if *n == 0 {
                ready.push(dependent.clone())
            }
        }
    }
    if visited != tasks.len() {
        return Err(WikiError::invalid("task dependency cycle"));
    }
    Ok(())
}
pub(super) fn bind(fs: &VaultFs, task: &TaskSpec) -> Result<()> {
    let bytes = read_bounded(
        fs,
        &task.input.path,
        crate::changes::prepare::MAX_PAYLOAD_BYTES,
    )?
    .ok_or_else(|| WikiError::new(ErrorCode::FreshnessConflict, "task descriptor missing"))?;
    if bytes.len() as u64 != task.input.byte_len || Blake3Hash::digest(&bytes) != task.input.hash {
        return Err(WikiError::new(
            ErrorCode::FreshnessConflict,
            "task descriptor changed",
        ));
    }
    for dep in &task.source_bindings {
        let actual = read_bounded(fs, &dep.path, crate::changes::prepare::MAX_PAYLOAD_BYTES)?
            .map_or(ExpectedState::Absent, |b| {
                ExpectedState::Hash(Blake3Hash::digest(b))
            });
        if actual != dep.expected {
            return Err(WikiError::new(
                ErrorCode::FreshnessConflict,
                "task source/config binding changed",
            ));
        }
    }
    Ok(())
}
pub(super) fn ready(tasks: &BTreeMap<Blake3Hash, TaskInspection>) -> Vec<TaskSpec> {
    let mut result = tasks
        .values()
        .filter(|t| {
            t.state == TaskState::Pending
                && t.spec
                    .dependencies
                    .iter()
                    .all(|d| tasks[d].state == TaskState::Completed)
        })
        .map(|t| t.spec.clone())
        .collect::<Vec<_>>();
    result.sort_by(|a, b| a.priority.cmp(&b.priority).then(a.key.cmp(&b.key)));
    result
}
pub(super) fn initial(specs: Vec<TaskSpec>) -> Result<BTreeMap<Blake3Hash, TaskInspection>> {
    let mut out = BTreeMap::new();
    for spec in specs {
        let key = spec.key.clone();
        if out
            .insert(
                key,
                TaskInspection {
                    spec,
                    state: TaskState::Pending,
                    outputs: vec![],
                    cache_outputs: vec![],
                    failure_code: None,
                },
            )
            .is_some()
        {
            return Err(WikiError::invalid("duplicate task key"));
        }
    }
    validate(&out)?;
    Ok(out)
}
/// Immutable genesis input binding; initial task order is part of the specification.
pub fn input_fingerprint(spec: &RunSpec) -> Result<Blake3Hash> {
    let value = serde_json::json!({"input_records":spec.scope.input_records,"source_snapshot":spec.scope.source_snapshot,"read_preconditions":spec.scope.read_preconditions,"tasks":spec.tasks.iter().map(|t|(&t.key,&t.input_hash)).collect::<Vec<_>>()});
    Ok(Blake3Hash::digest(serde_json::to_vec(&value).map_err(
        |_| WikiError::invalid("input fingerprint encoding"),
    )?))
}

//! Disposable owner-indexed hints. Markdown and selected original proofs remain
//! authority; neither indexed identities nor successful joins authenticate them.
use super::{DocumentRow, query::QuerySnapshot, query_types::QueryCatalog, sql};
use crate::{
    changes::prepare::strict_json,
    domain::*,
    jobs::{AttemptRef, Capability, DurableOutputRef, TaskSpec, TaskStage},
    records::parse_note,
    retrieval::{
        source_relation_store::{
            ANNOTATION_FENCE, Annotation, MAX_ARTIFACT_BYTES, MAX_DESCRIPTOR_BYTES, MAX_PATH_BYTES,
            MAX_SOURCE_BINDINGS, MAX_TASK_DEPENDENCIES, PreparationIdentity,
        },
        source_relations::{
            MAX_ANNOTATION_BYTES, MAX_EDGES, MAX_NODES, SCHEMA as RELATION_SCHEMA, SourceBinding,
        },
    },
    vault::ExpectedState,
};
use rusqlite::{Connection, Row, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const MAX_METADATA_BYTES: usize = 64 * 1024;
const ANNOTATION_SCHEMA: &str = "lwiki.source-relation-annotation.v1";
const GENERATION_SCHEMA: &str = "lwiki.source-relation-generation.v1";

pub(crate) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS relation_annotations(
 annotation_path TEXT COLLATE BINARY PRIMARY KEY,annotation_hash TEXT NOT NULL,
 source_id TEXT NOT NULL,revision_id TEXT NOT NULL,source_raw_hash TEXT NOT NULL,
 raw_path TEXT NOT NULL,generation_hash TEXT NOT NULL,metadata_json TEXT NOT NULL
 CHECK(length(CAST(metadata_json AS BLOB))<=65536));
CREATE INDEX IF NOT EXISTS relation_annotation_owners
 ON relation_annotations(source_id,revision_id,source_raw_hash,annotation_path);
CREATE TABLE IF NOT EXISTS relation_dependencies(
 annotation_path TEXT NOT NULL,path TEXT NOT NULL,expected_hash TEXT NOT NULL,
 PRIMARY KEY(annotation_path,path));
"#;

/// Untrusted locators/bindings only. No envelope, scopes, evidence or fact text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Candidate {
    pub annotation: DurableOutputRef,
    pub raw: DurableOutputRef,
    pub source: SourceBinding,
    pub task: TaskSpec,
    pub attempt: AttemptRef,
    pub preparation: PreparationIdentity,
}

/// Prepared once, then charged by the enclosing build/delta before any SQL.
/// Empty preparation still removes a previous hint for this document path.
pub(crate) struct ProjectedHint {
    path: VaultRelativePath,
    candidate: Option<Candidate>,
    metadata: String,
    storage_charges: Vec<usize>,
    storage_bytes: usize,
}
impl ProjectedHint {
    /// Conservative serialized table/index bytes, not physical SQLite pages.
    pub(crate) fn storage_bytes(&self) -> usize {
        self.storage_bytes
    }
    /// One charge per annotation/dependency row, including duplicated keys.
    /// Use these with the existing per-row/batch/cumulative admission policy.
    pub(crate) fn storage_charges(&self) -> &[usize] {
        &self.storage_charges
    }
}

fn byte_sum(parts: &[usize]) -> Result<usize> {
    parts.iter().try_fold(0usize, |sum, part| {
        sum.checked_add(*part).ok_or_else(|| {
            WikiError::new(ErrorCode::BudgetExceeded, "relation storage byte overflow")
        })
    })
}
fn annotation_storage(lengths: [usize; 8]) -> Result<usize> {
    // Table columns; annotation-path PK; owner-index Source/Revision/raw/path;
    // conservative per-record/index framing and rowid overhead.
    byte_sum(&[
        512,
        byte_sum(&lengths)?,
        lengths[0],
        lengths[2],
        lengths[3],
        lengths[4],
        lengths[0],
    ])
}
fn dependency_storage(lengths: [usize; 3]) -> Result<usize> {
    // Table columns plus duplicated (annotation_path,path) primary key.
    byte_sum(&[128, byte_sum(&lengths)?, lengths[0], lengths[1]])
}

fn corrupt(message: &str) -> WikiError {
    WikiError::new(ErrorCode::IndexCorrupt, message)
}
fn present(c: &Connection) -> Result<bool> {
    let count: i64 = c.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ('relation_annotations','relation_dependencies')",
        [], |row| row.get(0),
    ).map_err(sql::sql_error)?;
    match count {
        0 => Ok(false),
        2 => Ok(true),
        _ => Err(corrupt("incomplete source relation candidate layout")),
    }
}
/// Add the optional disposable layout only for an actual relation publication.
/// Partial layouts are corruption, never repaired implicitly.
pub(crate) fn ensure_for_publication(c: &Connection) -> Result<()> {
    if c.is_autocommit() {
        return Err(corrupt("relation layout requires publication transaction"));
    }
    if !present(c)? {
        c.execute_batch(SCHEMA).map_err(sql::sql_error)?;
    }
    Ok(())
}

fn artifact_id(attempt: &AttemptRef, schema: &str) -> Result<RecordId> {
    let bytes = crate::graph::packet::canonical_json(&(schema, attempt))?;
    RecordId::new(format!(
        "run_event_relations_{}",
        Blake3Hash::digest(bytes).hex()
    ))
}
fn artifact_path(attempt: &AttemptRef, id: &RecordId) -> Result<VaultRelativePath> {
    crate::retrieval::source_relation_store::artifact_path(&attempt.run_id, id)
}
fn shape(candidate: &Candidate) -> Result<()> {
    let task = &candidate.task;
    let preparation = &candidate.preparation;
    if task.stage != TaskStage::PrepareSourceRelations
        || task.capability != Some(Capability::Generate)
        || task.prompt_hash.as_ref() != Some(&preparation.prompt_hash)
        || task.schema_hash.as_ref() != Some(&preparation.schema_hash)
        || task.model_hash.as_ref() != Some(&preparation.model_identity_hash)
        || task.settings_hash != preparation.settings_hash
        || candidate.attempt.task_key != task.key
        || candidate.attempt.number == 0
        || task.input.byte_len == 0
        || task.input.byte_len > MAX_DESCRIPTOR_BYTES as u64
        || task.input.path.as_str().len() > MAX_PATH_BYTES
        || task.dependencies.len() > MAX_TASK_DEPENDENCIES
        || task.dependencies.iter().collect::<BTreeSet<_>>().len() != task.dependencies.len()
        || task.source_bindings.is_empty()
        || task.source_bindings.len() > MAX_SOURCE_BINDINGS
        || task.key != crate::jobs::tasks::task_key(task)?
        || candidate.annotation.record.vault_id != candidate.raw.record.vault_id
    {
        return Err(corrupt("source relation candidate task/binding differs"));
    }
    let mut paths = BTreeSet::new();
    for dependency in &task.source_bindings {
        if dependency.path.as_str().len() > MAX_PATH_BYTES
            || !paths.insert(&dependency.path)
            || !matches!(dependency.expected, ExpectedState::Hash(_))
        {
            return Err(corrupt("source relation candidate dependencies differ"));
        }
    }
    for (reference, schema) in [
        (&candidate.annotation, ANNOTATION_SCHEMA),
        (&candidate.raw, GENERATION_SCHEMA),
    ] {
        let id = artifact_id(&candidate.attempt, schema)?;
        if reference.record.expected_kind != RecordKind::RunEvent
            || reference.record.record_id != id
            || reference.path != artifact_path(&candidate.attempt, &id)?
            || reference.path.as_str().len() > MAX_PATH_BYTES
        {
            return Err(corrupt(
                "source relation candidate retained identity differs",
            ));
        }
    }
    Ok(())
}

fn projected(row: &DocumentRow) -> Result<Option<Candidate>> {
    if row.kind != Some(RecordKind::RunEvent)
        || row.raw_text.len() > MAX_ARTIFACT_BYTES
        || !row.raw_text.contains(ANNOTATION_FENCE)
    {
        return Ok(None);
    }
    // Projection already owns the DocumentRow. Parsing is bounded; this digest
    // is internal cache consistency, never selected proof or source authority.
    let note = parse_note(row.raw_text.as_bytes());
    let record = note
        .canonical
        .as_ref()
        .ok_or_else(|| corrupt("annotation RunEvent absent"))?;
    if Some(record.id()) != row.record_id.as_ref()
        || record.kind() != RecordKind::RunEvent
        || note.source_hash != row.hash
        || record.string("wiki_event_type") != Some(ANNOTATION_SCHEMA)
    {
        return Err(corrupt("annotation document identity differs"));
    }
    let body = crate::graph::packet::fenced_json(&note, ANNOTATION_FENCE, MAX_ARTIFACT_BYTES)?;
    let annotation: Annotation = strict_json(body)?;
    let envelope = &annotation.envelope;
    if annotation.schema != ANNOTATION_SCHEMA
        || envelope.schema != RELATION_SCHEMA
        || envelope.nodes.len() > MAX_NODES
        || envelope.edges.len() > MAX_EDGES
        || envelope.preparation.retained_generation_id != annotation.raw.record.record_id
        || envelope.preparation.retained_generation_hash != annotation.raw.hash
        || record.string("wiki_run_id") != Some(annotation.attempt.run_id.as_str())
        || note
            .fields
            .as_ref()
            .and_then(|fields| fields.get("wiki_sequence"))
            .and_then(serde_json::Value::as_u64)
            != Some(u64::from(annotation.attempt.number))
        || serde_json::to_vec(envelope)
            .map_err(|_| corrupt("annotation envelope cannot encode"))?
            .len()
            > MAX_ANNOTATION_BYTES
    {
        return Err(corrupt("annotation shape differs"));
    }
    let mut ids = BTreeSet::new();
    for node in &envelope.nodes {
        if !ids.insert(&node.id)
            || node.scope.trim().is_empty()
            || node.scope.len() > 256
            || node.scope.chars().any(char::is_control)
            || node.evidence.span.is_empty()
        {
            return Err(corrupt("annotation node shape differs"));
        }
    }
    let mut edges = BTreeSet::new();
    for edge in &envelope.edges {
        if !ids.contains(&edge.from)
            || !ids.contains(&edge.to)
            || edge.evidence.span.is_empty()
            || !edges.insert((&edge.from, &edge.to, edge.kind))
        {
            return Err(corrupt("annotation edge shape differs"));
        }
    }
    let p = &envelope.preparation;
    let candidate = Candidate {
        annotation: DurableOutputRef {
            record: RecordRef {
                vault_id: annotation.raw.record.vault_id.clone(),
                record_id: record.id().clone(),
                expected_kind: RecordKind::RunEvent,
            },
            path: row.path.clone(),
            hash: row.hash.clone(),
        },
        raw: annotation.raw,
        source: envelope.source.clone(),
        task: annotation.task,
        attempt: annotation.attempt,
        preparation: PreparationIdentity {
            recipe_id: p.recipe_id.clone(),
            recipe_hash: p.recipe_hash.clone(),
            prompt_hash: p.prompt_hash.clone(),
            schema_hash: p.schema_hash.clone(),
            model_identity_hash: p.model_identity_hash.clone(),
            settings_hash: p.settings_hash.clone(),
        },
    };
    shape(&candidate)?;
    Ok(Some(candidate))
}

/// Parses and serializes at most once. Caller admits additional storage before
/// applying this prepared value in its existing publication transaction.
pub(crate) fn prepare_document(row: &DocumentRow) -> Result<ProjectedHint> {
    let mut prepared = ProjectedHint {
        path: row.path.clone(),
        candidate: None,
        metadata: String::new(),
        storage_charges: vec![],
        storage_bytes: 0,
    };
    let Some(candidate) = projected(row).ok().flatten() else {
        return Ok(prepared);
    };
    let metadata =
        serde_json::to_string(&candidate).map_err(|_| corrupt("candidate cannot encode"))?;
    if metadata.len() > MAX_METADATA_BYTES {
        return Ok(prepared);
    }
    prepared.storage_charges.push(annotation_storage([
        candidate.annotation.path.as_str().len(),
        candidate.annotation.hash.as_str().len(),
        candidate.source.source_id.as_str().len(),
        candidate.source.source_revision.as_str().len(),
        candidate.source.raw_hash.as_str().len(),
        candidate.raw.path.as_str().len(),
        candidate.raw.hash.as_str().len(),
        metadata.len(),
    ])?);
    for dependency in &candidate.task.source_bindings {
        let ExpectedState::Hash(hash) = &dependency.expected else {
            unreachable!("shape checked");
        };
        prepared.storage_charges.push(dependency_storage([
            prepared.path.as_str().len(),
            dependency.path.as_str().len(),
            hash.as_str().len(),
        ])?);
    }
    prepared.storage_bytes = byte_sum(&prepared.storage_charges)?;
    prepared.candidate = Some(candidate);
    prepared.metadata = metadata;
    Ok(prepared)
}

/// Applies exactly the prepared bytes, without parsing or serialization. This
/// does not reserve storage: callers must admit storage_charges beforehand.
pub(crate) fn apply_document(c: &Connection, prepared: &ProjectedHint) -> Result<()> {
    if !present(c)? {
        return Ok(());
    }
    delete_present(c, &prepared.path)?;
    let Some(candidate) = &prepared.candidate else {
        return Ok(());
    };
    c.execute("INSERT INTO relation_annotations(annotation_path,annotation_hash,source_id,revision_id,source_raw_hash,raw_path,generation_hash,metadata_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        params![prepared.path.as_str(), candidate.annotation.hash.as_str(), candidate.source.source_id.as_str(), candidate.source.source_revision.as_str(), candidate.source.raw_hash.as_str(), candidate.raw.path.as_str(), candidate.raw.hash.as_str(), prepared.metadata]).map_err(sql::sql_error)?;
    for dependency in &candidate.task.source_bindings {
        let ExpectedState::Hash(hash) = &dependency.expected else {
            unreachable!("shape checked");
        };
        c.execute("INSERT INTO relation_dependencies(annotation_path,path,expected_hash) VALUES(?1,?2,?3)", params![prepared.path.as_str(), dependency.path.as_str(), hash.as_str()]).map_err(sql::sql_error)?;
    }
    Ok(())
}

/// Convenience for callers whose enclosing policy already reserves storage.
/// Build/delta integration uses prepare_document + explicit charge + apply.
pub(crate) fn project_document(c: &Connection, row: &DocumentRow) -> Result<()> {
    if !present(c)? {
        return Ok(());
    }
    apply_document(c, &prepare_document(row)?)
}

fn scalar_lengths<const N: usize>(row: &Row<'_>) -> Result<[usize; N]> {
    let mut lengths = [0usize; N];
    for (column, length) in lengths.iter_mut().enumerate() {
        *length = match row.get_ref(column).map_err(sql::sql_error)? {
            rusqlite::types::ValueRef::Text(bytes) | rusqlite::types::ValueRef::Blob(bytes) => {
                bytes.len()
            }
            rusqlite::types::ValueRef::Integer(_) | rusqlite::types::ValueRef::Real(_) => 8,
            rusqlite::types::ValueRef::Null => 0,
        };
    }
    Ok(lengths)
}

/// Charges the old annotation/dependency rows before replacement or deletion.
/// No metadata decode, ownership allocation, mutation or new allowance. Invoke
/// once per retired path; charge new prepared rows separately before applying.
pub(crate) fn charge_retired_document(
    c: &Connection,
    path: &VaultRelativePath,
    stats: &mut super::normalized_delta::DeltaStats,
) -> Result<()> {
    if !present(c)? {
        return Ok(());
    }
    let mut statement = c.prepare("SELECT annotation_path,annotation_hash,source_id,revision_id,source_raw_hash,raw_path,generation_hash,metadata_json FROM relation_annotations WHERE annotation_path=?1").map_err(sql::sql_error)?;
    let mut rows = statement.query([path.as_str()]).map_err(sql::sql_error)?;
    if let Some(row) = rows.next().map_err(sql::sql_error)? {
        let lengths = scalar_lengths::<8>(row)?;
        super::normalized_fact_delta::charge_old(annotation_storage(lengths)?, stats)?;
        for column in 0..8 {
            text(row, column)?;
        }
        if lengths[0] > MAX_PATH_BYTES
            || lengths[5] > MAX_PATH_BYTES
            || lengths[7] > MAX_METADATA_BYTES
        {
            return Err(corrupt("retired annotation scalar ceiling exceeded"));
        }
    }
    drop(rows);
    drop(statement);
    let mut statement = c.prepare("SELECT annotation_path,path,expected_hash FROM relation_dependencies WHERE annotation_path=?1 ORDER BY path LIMIT ?2").map_err(sql::sql_error)?;
    let mut rows = statement
        .query(params![path.as_str(), (MAX_SOURCE_BINDINGS + 1) as i64])
        .map_err(sql::sql_error)?;
    let mut dependencies = 0usize;
    while let Some(row) = rows.next().map_err(sql::sql_error)? {
        let lengths = scalar_lengths::<3>(row)?;
        super::normalized_fact_delta::charge_old(dependency_storage(lengths)?, stats)?;
        for column in 0..3 {
            text(row, column)?;
        }
        dependencies += 1;
        if dependencies > MAX_SOURCE_BINDINGS {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "retired relation dependency ceiling exceeded",
            ));
        }
        if lengths[0] > MAX_PATH_BYTES || lengths[1] > MAX_PATH_BYTES {
            return Err(corrupt("retired relation dependency path ceiling exceeded"));
        }
    }
    Ok(())
}
fn delete_present(c: &Connection, path: &VaultRelativePath) -> Result<()> {
    c.execute(
        "DELETE FROM relation_dependencies WHERE annotation_path=?1",
        [path.as_str()],
    )
    .map_err(sql::sql_error)?;
    c.execute(
        "DELETE FROM relation_annotations WHERE annotation_path=?1",
        [path.as_str()],
    )
    .map_err(sql::sql_error)?;
    Ok(())
}
pub(crate) fn delete_document(c: &Connection, path: &VaultRelativePath) -> Result<()> {
    if present(c)? {
        delete_present(c, path)?;
    }
    Ok(())
}

fn text<'a>(row: &'a Row<'_>, column: usize) -> Result<&'a str> {
    row.get_ref(column)
        .map_err(sql::sql_error)?
        .as_str()
        .map_err(|_| corrupt("relation candidate scalar is not text"))
}
const LOOKUP: &str = "SELECT a.annotation_path,a.annotation_hash,a.source_id,a.revision_id,a.source_raw_hash,a.raw_path,a.generation_hash,a.metadata_json,ann.id,raw.id,s.path,s.hash,r.path,r.hash \
 FROM relation_annotations a INDEXED BY relation_annotation_owners \
 JOIN documents d ON d.path=?1 AND d.file_hash=?2 AND d.source_id=a.source_id AND d.owner_revision=a.revision_id AND d.file_hash=a.source_raw_hash AND d.eligibility='current' \
 JOIN records s ON s.id=a.source_id AND s.kind='source' AND s.eligibility='current' \
 JOIN relation_dependencies sd ON sd.annotation_path=a.annotation_path AND sd.path=s.path AND sd.expected_hash=s.hash \
 JOIN records r ON r.id=a.revision_id AND r.kind='revision' AND r.eligibility='current' \
 JOIN relation_dependencies rd ON rd.annotation_path=a.annotation_path AND rd.path=r.path AND rd.expected_hash=r.hash \
 JOIN records ann ON ann.path=a.annotation_path AND ann.hash=a.annotation_hash AND ann.kind='run_event' \
 JOIN records raw ON raw.path=a.raw_path AND raw.hash=a.generation_hash AND raw.kind='run_event' \
 WHERE a.source_id=?3 AND a.revision_id=?4 AND a.source_raw_hash=?2 ORDER BY a.annotation_path LIMIT ?5";

fn candidates(
    c: &Connection,
    owner: &DocumentRow,
    vault: &RecordId,
    limit: usize,
    mut reserve: impl FnMut(&Row<'_>, usize) -> Result<()>,
) -> Result<Vec<Candidate>> {
    if !(1..=80).contains(&limit) {
        return Err(WikiError::invalid(
            "relation candidate limit requires 1..=80",
        ));
    }
    if owner.eligibility != Eligibility::Current
        || owner.source_id.is_none()
        || owner.owner_revision.is_none()
    {
        return Ok(vec![]);
    }
    // Old v3 caches can lack both tables. Inspect only the named capability,
    // reserve the scalar result, and preserve ordinary retrieval fallback.
    let mut statement = c.prepare("SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ('relation_annotations','relation_dependencies')").map_err(sql::sql_error)?;
    let mut rows = statement.query([]).map_err(sql::sql_error)?;
    let row = rows
        .next()
        .map_err(sql::sql_error)?
        .ok_or_else(|| corrupt("relation capability count absent"))?;
    reserve(row, 1)?;
    match row.get::<_, i64>(0).map_err(sql::sql_error)? {
        0 => return Ok(vec![]),
        2 => {}
        _ => return Err(corrupt("incomplete relation candidate layout")),
    }
    drop(rows);
    drop(statement);
    let mut statement = c.prepare(LOOKUP).map_err(sql::sql_error)?;
    let mut rows = statement
        .query(params![
            owner.path.as_str(),
            owner.hash.as_str(),
            owner.source_id.as_ref().map(RecordId::as_str),
            owner.owner_revision.as_ref().map(RecordId::as_str),
            (limit + 1) as i64
        ])
        .map_err(sql::sql_error)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().map_err(sql::sql_error)? {
        reserve(row, 14)?;
        if out.len() == limit {
            return Err(WikiError::new(
                ErrorCode::BudgetExceeded,
                "source relation candidate limit exceeded",
            ));
        }
        let metadata = text(row, 7)?;
        if metadata.len() > MAX_METADATA_BYTES {
            return Err(corrupt("source relation metadata exceeds 64 KiB"));
        }
        let candidate: Candidate = strict_json(metadata.as_bytes())
            .map_err(|_| corrupt("invalid source relation metadata"))?;
        shape(&candidate)?;
        if candidate.annotation.path.as_str() != text(row, 0)?
            || candidate.annotation.hash.as_str() != text(row, 1)?
            || candidate.source.source_id.as_str() != text(row, 2)?
            || candidate.source.source_revision.as_str() != text(row, 3)?
            || candidate.source.raw_hash.as_str() != text(row, 4)?
            || candidate.raw.path.as_str() != text(row, 5)?
            || candidate.raw.hash.as_str() != text(row, 6)?
            || &candidate.annotation.record.vault_id != vault
            || candidate.annotation.record.record_id.as_str() != text(row, 8)?
            || candidate.raw.record.record_id.as_str() != text(row, 9)?
            || out
                .last()
                .is_some_and(|last: &Candidate| last.annotation.path >= candidate.annotation.path)
        {
            return Err(corrupt("source relation indexed identity differs"));
        }
        for (path_column, hash_column) in [(10, 11), (12, 13)] {
            let path = text(row, path_column)?;
            let hash = text(row, hash_column)?;
            if !candidate.task.source_bindings.iter().any(|dependency| {
                dependency.path.as_str() == path && matches!(&dependency.expected, ExpectedState::Hash(expected) if expected.as_str() == hash)
            }) { return Err(corrupt("source relation dependency hint differs from task")); }
        }
        out.push(candidate);
    }
    Ok(out)
}
impl QuerySnapshot {
    pub(crate) fn source_relation_candidates(
        &self,
        owner: &DocumentRow,
        limit: usize,
    ) -> Result<Vec<Candidate>> {
        self.check_query_budget()?;
        let result = candidates(
            self.connection(),
            owner,
            self.vault_id(),
            limit,
            |row, columns| {
                self.check_query_budget()?;
                self.reserve_scalar_row(row, columns)
            },
        )?;
        self.check_query_budget()?;
        Ok(result)
    }
}

#[cfg(test)]
#[path = "source_relations_tests.rs"]
mod tests;

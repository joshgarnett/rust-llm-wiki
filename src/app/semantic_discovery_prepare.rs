//! Cache-only discovery preparation; canonical writes only invalidate its interval.
use super::*;
use crate::{
    catalog::query_types::{QueryCatalog, QueryReadLimits},
    retrieval::{
        discovery::{
            self,
            storage::{BuildState, GraphReader},
            *,
        },
        unit_inventory_types::*,
        vectors::VectorReadBudget,
    },
};
use std::time::Instant;

const MAX_OWNER_PAGES: usize = 32;
use discovery::admission::{DiscoveryAdmission, DiscoveryMaintenanceLimits, StorageGeometry};
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    format: u32,
    space: Blake3Hash,
    policy: RenderPolicyId,
    incarnation: Blake3Hash,
    through: u64,
    since: u64,
    eligible: usize,
    expected: u32,
    dimensions: u32,
    kind: GraphKind,
    retained_base: Option<GraphRef>,
}
impl Plan {
    fn fingerprint(&self) -> Result<Blake3Hash> {
        Ok(Blake3Hash::digest(crate::graph::packet::canonical_json(
            &(
                "lwiki-semantic-preparation-v1",
                "modified_seq,owner,unit_id",
                discovery::algorithm_identity(),
                self,
            ),
        )?))
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stage {
    plan: Plan,
    admission: DiscoveryAdmission,
    attempt: Blake3Hash,
    input: BuildState,
    after: Option<UnitOwnerCursor>,
    input_complete: bool,
    input_root: Option<GraphRef>,
    output_root: Option<GraphRef>,
    output_digest: Option<Blake3Hash>,
    audit: Option<discovery::audit::AuditCursor>,
}
impl Stage {
    fn audit_scope(&self) -> discovery::audit::AuditScope {
        discovery::audit::AuditScope {
            space: self.plan.space.clone(),
            policy: self.plan.policy.clone(),
            incarnation: self.plan.incarnation.clone(),
            required_seq: self.plan.through,
            since_seq: self.plan.since,
            expected_nodes: self.plan.expected,
        }
    }
    fn validate(&self, expected: &Plan) -> Result<()> {
        let fingerprint = expected.fingerprint()?;
        if self.plan.fingerprint()? != fingerprint {
            return Err(conflict("graph continuation plan changed"));
        }
        self.admission.validate()?;
        if self.admission.plan_hash != fingerprint
            || self.admission.node_count != expected.expected
            || self.admission.dimensions != expected.dimensions
            || self.admission.limits != DiscoveryMaintenanceLimits::default()
        {
            return Err(discovery::corrupt(
                "graph stage admission differs from plan",
            ));
        }
        self.input
            .validate_resume(&fingerprint, self.admission.logical_tree_cap_bytes)?;
        if self.input.kind != GraphKind::StagingInput
            || self.input.expected_nodes != expected.expected
            || self.input.dimensions != expected.dimensions
            || self.input.max_level != 0
            || self.input.entry != (expected.expected > 0).then_some(0)
            || (self.input.next_node > 0 && self.after.is_none())
            || self.after.as_ref().is_some_and(|a| {
                a.modified_seq <= expected.since || a.modified_seq > expected.through
            })
            || (self.input_complete && self.input.next_node != expected.expected)
            || (self.input_root.is_some() != (self.input_complete && expected.expected > 0))
            || (self.output_root.is_some() != self.output_digest.is_some())
            || (self.output_root.is_some() && self.input_root.is_none())
            || (self.audit.is_some() && self.output_root.is_none())
        {
            return Err(discovery::corrupt(
                "graph continuation phase or geometry differs",
            ));
        }
        for (root, kind) in [
            (self.input_root.as_ref(), GraphKind::StagingInput),
            (self.output_root.as_ref(), expected.kind),
        ] {
            if let Some(root) = root {
                if root.kind != kind
                    || root.format != FORMAT
                    || root.dimensions != expected.dimensions
                    || root.node_count != expected.expected
                    || root.max_level > MAX_LEVEL
                {
                    return Err(discovery::corrupt(
                        "graph continuation root differs from its plan",
                    ));
                }
            }
        }
        if self
            .input_root
            .as_ref()
            .is_some_and(|r| r.entry != Some(0) || r.max_level != 0)
        {
            return Err(discovery::corrupt("graph staged input topology differs"));
        }
        Ok(())
    }
}

fn require_current(catalog: &Catalog, stage: &Stage) -> Result<()> {
    let reader = catalog.cached_query_snapshot(QueryReadLimits::default())?;
    let current = reader
        .discovery_state(&stage.plan.space, &stage.plan.policy)?
        .ok_or_else(|| conflict("graph registration disappeared"))?;
    if current.required_seq != stage.plan.through
        || catalog_incarnation(reader.vault_id(), reader.snapshot())? != stage.plan.incarnation
    {
        return Err(conflict("graph preparation interval changed before debit"));
    }
    reader.verify_operations(catalog)
}
fn conflict(message: &str) -> WikiError {
    WikiError::new(ErrorCode::FreshnessConflict, message)
}
fn phase(deadline: Instant) -> Result<std::rc::Rc<VectorReadBudget>> {
    VectorReadBudget::new(deadline.min(Instant::now() + Duration::from_secs(30)))
}
fn save(catalog: &Catalog, writer: &WriterPermit, stage: &Stage) -> Result<()> {
    catalog.save_discovery_stage(
        writer,
        &stage.plan.space,
        &stage.plan.policy,
        &stage.plan.incarnation,
        stage.plan.through,
        &serde_json::to_value(stage).map_err(|_| conflict("graph stage encoding"))?,
    )
}
fn pending(stage: &Stage, phase: &str) -> serde_json::Value {
    serde_json::json!({"published":false,"phase":phase,
        "eligible_current_units":stage.plan.eligible,
        "staged_interval_units":stage.input.next_node,
        "expected_interval_units":stage.plan.expected,
        "through_seq":stage.plan.through,
        "logical_input_bytes":stage.input.written_bytes,
        "reserved_artifact_bytes":stage.admission.reserved_artifact_bytes,
        "accounting_attempt":stage.attempt,
        "child_address_space_limit_bytes":stage.admission.limits.max_child_address_space_bytes,
        "physical_peak_disk_bytes":null})
}
impl OfflineApp {
    pub(super) fn prepare_semantic_discovery(
        &self,
        settings: &EmbeddingSettings,
        space: &Blake3Hash,
        deadline: Instant,
    ) -> Result<serde_json::Value> {
        let deadline = deadline.min(
            Instant::now()
                + Duration::from_millis(DiscoveryMaintenanceLimits::default().max_invocation_ms),
        );
        let catalog = Catalog::new(self.fs.clone(), self.vault_id.clone());
        let writer = self.embedding_writer()?;
        let reader = catalog.cached_query_snapshot(QueryReadLimits::default())?;
        let policy = RenderPolicyId::for_settings(&reader.snapshot().parser_fingerprint, settings)?;
        let incarnation = catalog_incarnation(reader.vault_id(), reader.snapshot())?;
        drop(reader);
        let selected = catalog.register_discovery(&writer, space, &policy)?;
        let store = VectorStore::open_bounded_snapshot(&self.fs, &phase(deadline)?)?;
        let active = store
            .active()?
            .ok_or_else(|| conflict("graph preparation requires retained active vectors"))?;
        if &active.id != space || &active.spec.settings != settings {
            return Err(conflict("graph preparation active space changed"));
        }
        let dimensions = active
            .actual_dimensions
            .ok_or_else(|| conflict("graph dimensions absent"))?;
        drop(store);
        if let Some(manifest) = selected
            .manifest
            .as_ref()
            .filter(|m| m.through_seq == selected.required_seq)
        {
            return Ok(
                serde_json::json!({"published":true,"phase":"ready", "coverage":manifest.coverage,
                "through_seq":manifest.through_seq,"format":manifest.format}),
            );
        }

        let reader = catalog.cached_query_snapshot(QueryReadLimits::default())?;
        let retained = selected
            .manifest
            .as_ref()
            .filter(|m| m.incarnation == incarnation);
        let since = retained.map_or(0, |m| m.base_through_seq);
        let changed = reader.unit_changed_count(&policy, since)?;
        let (kind, since, expected, retained_base) = if changed <= MAX_DELTA {
            (
                GraphKind::Delta,
                since,
                changed,
                retained.and_then(|m| m.base.clone()),
            )
        } else {
            (GraphKind::Base, 0, selected.eligible_current_units, None)
        };
        let plan = Plan {
            format: FORMAT,
            space: space.clone(),
            policy,
            incarnation,
            through: selected.required_seq,
            since,
            eligible: selected.eligible_current_units,
            expected: u32::try_from(expected)
                .map_err(|_| discovery::exhausted("graph node count exceeds format"))?,
            dimensions,
            kind,
            retained_base,
        };
        reader.verify_operations(&catalog)?;
        drop(reader);
        let fingerprint = plan.fingerprint()?;
        let admission = DiscoveryAdmission::derive(
            fingerprint.clone(),
            plan.expected,
            plan.dimensions,
            DiscoveryMaintenanceLimits::default(),
            StorageGeometry::observe(&self.fs)?,
        )?;
        let mut stage = match catalog.discovery_stage(
            &writer,
            space,
            &plan.policy,
            &plan.incarnation,
            plan.through,
        )? {
            Some(value) => {
                let stage: Stage = serde_json::from_value(value)
                    .map_err(|_| discovery::corrupt("graph continuation encoding invalid"))?;
                stage.validate(&plan)?;
                if stage.admission != admission {
                    return Err(conflict(
                        "graph storage geometry or resource policy changed",
                    ));
                }
                discovery::accounting::verify(&self.fs, &writer, &stage.attempt, &admission)?;
                stage
            }
            None => Stage {
                attempt: discovery::accounting::reserve(&self.fs, &writer, &admission)?,
                admission,
                input: BuildState::new(
                    fingerprint.clone(),
                    GraphKind::StagingInput,
                    dimensions,
                    plan.expected,
                    (plan.expected > 0).then_some(0),
                    0,
                )?,
                plan,
                after: None,
                input_complete: false,
                input_root: None,
                output_root: None,
                output_digest: None,
                audit: None,
            },
        };
        // Persist the attempt before the first graph write. A crash before this
        // save retains its whole reservation as an unknown, never refunded hold.
        save(&catalog, &writer, &stage)?;
        let had_complete_input = stage.input_complete;
        drop(writer);
        // Writer ownership covers one bounded staging page. Native construction
        // and read-only audit do not retain canonical writer authority.
        for _ in 0..MAX_OWNER_PAGES {
            if stage.input_complete || Instant::now() >= deadline {
                break;
            }
            let writer = self.embedding_writer()?;
            let read_budget = phase(deadline)?;
            let reader = catalog.cached_query_snapshot(QueryReadLimits {
                max_elapsed_ms: read_budget.remaining_ms()?,
                ..Default::default()
            })?;
            let current = reader
                .discovery_state(space, &stage.plan.policy)?
                .ok_or_else(|| conflict("graph registration disappeared"))?;
            if current.required_seq != stage.plan.through
                || catalog_incarnation(reader.vault_id(), reader.snapshot())?
                    != stage.plan.incarnation
            {
                return Err(conflict("graph preparation interval changed"));
            }
            // Paged complete owner groups; no owner split is selected.
            let bindings = reader.unit_owner_bindings_page(
                &stage.plan.policy,
                stage.after.as_ref(),
                stage.plan.since,
                stage.plan.through,
                128,
            )?;
            if bindings.is_empty() {
                if stage.input.next_node != stage.plan.expected {
                    return Err(discovery::corrupt("graph prepared interval count differs"));
                }
                stage.input_complete = true;
                if stage.plan.expected > 0 {
                    let mut credit = discovery::accounting::debit(
                        &self.fs,
                        &writer,
                        &stage.attempt,
                        &stage.admission,
                        stage.input.finish_charge(&stage.admission)?,
                    )?;
                    stage.input_root = Some(stage.input.finish_admitted(
                        &self.fs,
                        &writer,
                        &fingerprint,
                        stage.admission.logical_tree_cap_bytes,
                        deadline,
                        &mut credit,
                    )?);
                }
                save(&catalog, &writer, &stage)?;
                break;
            }
            for binding in &bindings {
                if Instant::now() >= deadline {
                    break;
                }
                if binding.unit_count > 3960
                    || binding.unit_count.saturating_mul(dimensions as usize * 4) > 64 * 1024 * 1024
                    || binding.unit_count.saturating_mul(MAX_NODE_METADATA) > 64 * 1024 * 1024
                {
                    return Err(discovery::exhausted(
                        "complete graph owner group exceeds maintenance page",
                    ));
                }
                if binding.unit_count.saturating_add(1)
                    > 4096usize.saturating_sub(reader.usage().rows)
                {
                    break;
                }
                // Each complete owner has one independently bounded maintenance
                // phase; prior owners cannot consume its residual byte allowance.
                let owner_budget = phase(deadline)?;
                let available = VectorStore::open_bounded_snapshot(&self.fs, &owner_budget)?;
                if !binding.tombstone && !available.owner_binding_ready(space, binding)? {
                    return Err(conflict("graph owner needs retained vector acknowledgment"));
                }
                let descriptors = if binding.tombstone {
                    Vec::new()
                } else {
                    reader.unit_descriptors_for_owner(&stage.plan.policy, &binding.owner, 4090)?
                };
                if descriptors.len() != binding.unit_count {
                    return Err(conflict("graph owner unit count changed"));
                }
                let mut credit = discovery::accounting::debit(
                    &self.fs,
                    &writer,
                    &stage.attempt,
                    &stage.admission,
                    stage
                        .input
                        .append_charge(binding.unit_count as u32, &stage.admission)?,
                )?;
                let mut next = stage.input.clone();
                for descriptor in descriptors {
                    let vector = available
                        .vector(space, &descriptor.input_hash)?
                        .ok_or_else(|| {
                            WikiError::new(
                                ErrorCode::OfflineUnavailable,
                                "graph preparation requires all retained vectors",
                            )
                        })?;
                    next.append_admitted(
                        &self.fs,
                        &writer,
                        GraphNode {
                            id: next.next_node,
                            descriptor,
                            binding: binding.clone(),
                            vector,
                            neighbors: Vec::new(),
                        },
                        &fingerprint,
                        stage.admission.logical_tree_cap_bytes,
                        deadline,
                        &mut credit,
                    )?;
                }
                reader.verify_operations(&catalog)?;
                stage.input = next;
                stage.after = Some(UnitOwnerCursor {
                    modified_seq: binding.modified_seq,
                    owner: binding.owner.clone(),
                });
            }
            save(&catalog, &writer, &stage)?;
        }
        if !stage.input_complete {
            return Ok(pending(&stage, "staging_inputs"));
        }
        if stage.plan.expected == 0 {
            let writer = self.embedding_writer()?;
            let active = VectorStore::open_bounded_snapshot(&self.fs, &phase(deadline)?)?
                .active()?
                .ok_or_else(|| conflict("graph publication active space absent"))?;
            if active.id != stage.plan.space
                || active.actual_dimensions != Some(stage.plan.dimensions)
            {
                return Err(conflict("graph publication active space changed"));
            }
            let manifest = manifest(&stage, None);
            catalog.publish_discovery(&writer, &manifest)?;
            return Ok(
                serde_json::json!({"published":true,"phase":"ready","coverage":manifest.coverage}),
            );
        }
        if stage.plan.kind == GraphKind::Base && !had_complete_input {
            return Ok(pending(&stage, "awaiting_build"));
        }
        let input = stage
            .input_root
            .as_ref()
            .ok_or_else(|| discovery::corrupt("complete graph input root absent"))?
            .clone();
        if stage.output_root.is_none() {
            if Instant::now() >= deadline
                || (stage.plan.kind == GraphKind::Base
                    && deadline.saturating_duration_since(Instant::now()) < Duration::from_secs(90))
            {
                return Ok(pending(&stage, "awaiting_build"));
            }
            let mut output = BuildState::new(
                fingerprint.clone(),
                stage.plan.kind,
                dimensions,
                stage.plan.expected,
                Some(0),
                MAX_LEVEL,
            )?;
            let mut digest = Blake3Hash::digest("lwiki-semantic-export-v1");
            if stage.plan.kind == GraphKind::Delta {
                let writer = self.embedding_writer()?;
                require_current(&catalog, &stage)?;
                let mut credit = discovery::accounting::debit(
                    &self.fs,
                    &writer,
                    &stage.attempt,
                    &stage.admission,
                    output.append_charge(input.node_count, &stage.admission)?,
                )?;
                let mut graph = GraphReader::new(&self.fs, input.clone())?;
                let mut meter = DiscoveryMeter::new(phase(deadline)?);
                for id in 0..input.node_count {
                    let mut node = graph.node(id, &mut meter)?;
                    node.neighbors = vec![Vec::new()];
                    digest = discovery::audit::export_digest(&digest, &node)?;
                    output.append_admitted(
                        &self.fs,
                        &writer,
                        node,
                        &fingerprint,
                        stage.admission.logical_tree_cap_bytes,
                        deadline,
                        &mut credit,
                    )?;
                }
                output.max_level = 0;
            } else {
                // At most64 fully bounded nodes wait for one short writer batch.
                let mut buffered = Vec::with_capacity(64);
                let mut flush = |buffered: &mut Vec<GraphNode>| -> Result<()> {
                    if buffered.is_empty() {
                        return Ok(());
                    }
                    let writer = self.embedding_writer()?;
                    require_current(&catalog, &stage)?;
                    let mut credit = discovery::accounting::debit(
                        &self.fs,
                        &writer,
                        &stage.attempt,
                        &stage.admission,
                        output.append_charge(buffered.len() as u32, &stage.admission)?,
                    )?;
                    for node in buffered.drain(..) {
                        output.append_admitted(
                            &self.fs,
                            &writer,
                            node,
                            &fingerprint,
                            stage.admission.logical_tree_cap_bytes,
                            deadline,
                            &mut credit,
                        )?;
                    }
                    Ok(())
                };
                let result = discovery::process::build(
                    &self.fs,
                    &input,
                    &fingerprint,
                    &stage.admission,
                    deadline,
                    |node| {
                        digest = discovery::audit::export_digest(&digest, &node)?;
                        buffered.push(node);
                        if buffered.len() == 64 {
                            flush(&mut buffered)?;
                        }
                        Ok(())
                    },
                )?;
                flush(&mut buffered)?;
                drop(flush);
                if result.output_digest != digest || result.node_count != input.node_count {
                    return Err(discovery::corrupt("completed native graph output differs"));
                }
                output.entry = result.entry;
                output.max_level = result.max_level;
            }
            let writer = self.embedding_writer()?;
            require_current(&catalog, &stage)?;
            let mut credit = discovery::accounting::debit(
                &self.fs,
                &writer,
                &stage.attempt,
                &stage.admission,
                output.finish_charge(&stage.admission)?,
            )?;
            stage.output_root = Some(output.finish_admitted(
                &self.fs,
                &writer,
                &fingerprint,
                stage.admission.logical_tree_cap_bytes,
                deadline,
                &mut credit,
            )?);
            stage.output_digest = Some(digest.clone());
            let output = stage.output_root.as_ref().expect("just finished graph");
            let scope = stage.audit_scope();
            stage.audit = Some(discovery::audit::AuditCursor::new(&scope, &input, output)?);
            catalog.begin_discovery_audit(
                &writer,
                &scope,
                &input,
                output,
                &digest,
                &serde_json::to_value(&stage)
                    .map_err(|_| conflict("graph audit stage encoding"))?,
            )?;
        }
        // Resumable full durable audit is performed by the leaf validator; the
        // catalog selection remains the sole publication point after completion.
        self.audit_semantic_stage(&catalog, &mut stage, deadline)
    }
}
fn manifest(stage: &Stage, output: Option<GraphRef>) -> DiscoveryManifest {
    DiscoveryManifest {
        format: FORMAT,
        algorithm: discovery::algorithm_identity(),
        space: stage.plan.space.clone(),
        policy: stage.plan.policy.clone(),
        incarnation: stage.plan.incarnation.clone(),
        through_seq: stage.plan.through,
        base_through_seq: if stage.plan.kind == GraphKind::Base {
            stage.plan.through
        } else {
            stage.plan.since
        },
        base: if stage.plan.kind == GraphKind::Base {
            output.clone()
        } else {
            stage.plan.retained_base.clone()
        },
        delta: if stage.plan.kind == GraphKind::Delta {
            output
        } else {
            None
        },
        coverage: DiscoveryCoverage {
            eligible_current_units: stage.plan.eligible,
            compatible_paid_units: stage.plan.eligible,
            indexed_current_units: stage.plan.eligible,
        },
    }
}

impl OfflineApp {
    fn audit_semantic_stage(
        &self,
        catalog: &Catalog,
        stage: &mut Stage,
        deadline: Instant,
    ) -> Result<serde_json::Value> {
        let input = stage
            .input_root
            .clone()
            .ok_or_else(|| discovery::corrupt("graph audit input absent"))?;
        let output = stage
            .output_root
            .clone()
            .ok_or_else(|| discovery::corrupt("graph audit output absent"))?;
        let expected = stage
            .output_digest
            .clone()
            .ok_or_else(|| discovery::corrupt("graph audit digest absent"))?;
        let scope = stage.audit_scope();
        if stage.audit.is_none() {
            return Err(discovery::corrupt("graph audit authority cursor missing"));
        }
        for _ in 0..32 {
            if Instant::now() >= deadline {
                break;
            }
            let prior = stage.audit.as_ref().expect("audit cursor required").clone();
            {
                let writer = self.embedding_writer()?;
                catalog
                    .verify_discovery_audit(&writer, &scope, &input, &output, &expected, &prior)?;
            }
            let done = discovery::audit::audit_page(
                catalog,
                &scope,
                &input,
                &output,
                stage.audit.as_mut().expect("initialized audit"),
                deadline,
            )?;
            let writer = self.embedding_writer()?;
            catalog.advance_discovery_audit(
                &writer,
                &scope,
                &input,
                &output,
                &expected,
                &prior,
                stage.audit.as_ref().expect("successful audit page"),
                &serde_json::to_value(&*stage)
                    .map_err(|_| conflict("graph audit stage encoding"))?,
            )?;
            if done {
                stage
                    .audit
                    .as_ref()
                    .expect("completed audit")
                    .finish(&expected)?;
                let active = VectorStore::open_bounded_snapshot(&self.fs, &phase(deadline)?)?
                    .active()?
                    .ok_or_else(|| conflict("graph publication active space absent"))?;
                if active.id != stage.plan.space
                    || active.actual_dimensions != Some(stage.plan.dimensions)
                {
                    return Err(conflict("graph publication active space changed"));
                }
                let value = manifest(stage, Some(output));
                catalog.publish_discovery(&writer, &value)?;
                return Ok(
                    serde_json::json!({"published":true,"phase":"ready","coverage":value.coverage,
                    "through_seq":value.through_seq,"format":value.format}),
                );
            }
        }
        Ok(pending(stage, "auditing_export"))
    }
}
